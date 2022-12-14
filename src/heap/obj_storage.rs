use crate::{
    object::{Handle, HeapObjectHeader, AtomicHandle},
    traits::Object,
};
use atomic::Ordering;
use std::{
    cell::Cell,
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize, AtomicIsize},
};

use super::align_up;

/// ObjStorage supports management of off-heap references to objects allocated
/// in the RSGC heap.  An ObjStorage object provides a set of Java object
/// references (obj values), which clients refer to via obj* handles to the
/// associated ObjStorage entries.  Clients allocate entries to create a
/// (possibly weak) reference to a Java object, use that reference, and release
/// the reference when no longer needed.
///
/// The garbage collector must know about all ObjStorage objects and their
/// reference strength.  ObjStorage provides the garbage collector with support
/// for iteration over all the allocated entries.
///
/// There are several categories of interaction with an ObjStorage object.
///
/// (1) allocation and release of entries, by the mutator or the VM.
/// (2) iteration by the garbage collector, possibly concurrent with mutator.
/// (3) iteration by other, non-GC, tools (only at safepoints).
/// (4) cleanup of unused internal storage, possibly concurrent with mutator.
///
/// A goal of ObjStorage is to make these interactions thread-safe, while
/// minimizing potential lock contention issues within and between these
/// categories.  In particular, support for concurrent iteration by the garbage
/// collector, under certain restrictions, is required.  Further, it must not
/// block nor be blocked by other operations for long periods.
///
/// Internally, ObjStorage is a set of Block objects, from which entries are
/// allocated and released.  A block contains an obj[] and a bitmask indicating
/// which entries are in use (have been allocated and not yet released).  New
/// blocks are constructed and added to the storage object when an entry
/// allocation request is made and there are no blocks with unused entries.
/// Blocks may be removed and deleted when empty.
///
/// There are two important (and somewhat intertwined) protocols governing
/// concurrent access to a storage object.  These are the Concurrent Iteration
/// Protocol and the Allocation Protocol.  See the ParState class for a
/// discussion of concurrent iteration and the management of thread
/// interactions for this protocol.  Similarly, see the allocate() function for
/// a discussion of allocation.
pub struct ObjStorage {}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum EntryStatus {
    Invalid,
    Unallocated,
    Allocated,
}

pub const BULK_ALLOCATE_LIMIT: usize = size_of::<usize>() * 8;

#[derive(Clone, PartialEq, Eq)]
/// A Block has an embedded AllocationListEntry to provide the links between
/// Blocks in an AllocationList.
pub struct AllocationListEntry {
    prev: Cell<*mut Block>,
    next: Cell<*mut Block>,
}

impl AllocationListEntry {
    pub const fn new() -> Self {
        Self {
            prev: Cell::new(std::ptr::null_mut()),
            next: Cell::new(std::ptr::null_mut()),
        }
    }
}

impl Drop for AllocationListEntry {
    fn drop(&mut self) {
        assert!(self.prev.get().is_null(), "deleting attached block");
        assert!(self.next.get().is_null(), "deleting attached block");
    }
}

/// Fixed-sized array of objs, plus bookkeeping data.
/// All blocks are in the storage's _active_array, at the block's _active_index.
/// Non-full blocks are in the storage's _allocation_list, linked through the
/// block's _allocation_list_entry.  Empty blocks are at the end of that list.
#[repr(C)]
pub struct Block {
    data: [AtomicHandle<dyn Object>; BULK_ALLOCATE_LIMIT],
    allocated_bitmask: AtomicUsize,
    owner_address: usize,
    memory: *mut u8,
    active_index: usize,
    allocation_list_entry: AllocationListEntry,
    deferred_updates_next: AtomicPtr<Block>,
    release_refcount: AtomicUsize,
}

impl Block {
    pub fn allocation_list_entry(&self) -> &AllocationListEntry {
        &self.allocation_list_entry
    }

    pub fn check_index(&self, index: usize) {
        assert!(index < self.data.len(), "Index out of bounds: {}", index);
    }

    pub fn get_pointer(&self, index: usize) -> AtomicHandle<dyn Object> {
        self.check_index(index);
        self.data[index]
    }

    pub fn allocated_bitmask(&self) -> usize {
        self.allocated_bitmask.load(atomic::Ordering::Relaxed)
    }

    pub fn bitmask_for_index(&self, index: usize) -> usize {
        self.check_index(index);
        1 << index
    }

    pub fn iterate<F>(&self, mut f: F) -> bool
    where
        F: FnMut(AtomicHandle<dyn Object>) -> bool,
    {
        let mut bitmask = self.allocated_bitmask();
        while bitmask != 0 {
            let index = bitmask.trailing_zeros() as usize;
            let obj = self.get_pointer(index);
            if !f(obj) {
                return false;
            }
            bitmask &= !(1 << index);
        }

        true
    }
}

/// Array of all active blocks.  Refcounted for lock-free reclaim of
/// old array when a new array is allocated for expansion.
#[repr(C)]
pub struct ActiveArray {
    pub size: usize,
    pub block_count: AtomicUsize,
    pub refcount: AtomicIsize,
}

impl ActiveArray {
    pub const fn blocks_offset() -> usize {
        align_up(size_of::<Self>(), size_of::<Block>())
    }

    #[inline]
    pub fn base_ptr(&self) -> *mut *mut Block {
        let this = self as *const Self as usize;
        (this + Self::blocks_offset()) as _
    }

    #[inline]
    pub fn block_ptr(&self, index: usize) -> *mut *mut Block {
        unsafe { self.base_ptr().add(index) }
    }

    #[inline]
    pub fn at(&self, index: usize) -> *mut Block {
        assert!(index < self.block_count.load(atomic::Ordering::Relaxed));
        unsafe { *self.block_ptr(index) }
    }
}
/// Doubly-linked list of Blocks.  For all operations with a block
/// argument, the block must be from the list's ObjStorage.
pub struct AllocationList {
    head: *mut Block,
    tail: *mut Block,
}

impl AllocationList {
    pub const fn head(&self) -> *mut Block {
        self.head
    }

    pub const fn tail(&self) -> *mut Block {
        self.tail
    }

    pub fn prev(&self, block: *mut Block) -> *mut Block {
        unsafe { (*block).allocation_list_entry.prev.get() }
    }

    pub fn next(&self, block: *mut Block) -> *mut Block {
        unsafe { (*block).allocation_list_entry.next.get() }
    }

    pub const fn new() -> Self {
        Self {
            head: std::ptr::null_mut(),
            tail: std::ptr::null_mut(),
        }
    }

    pub fn push_front(&mut self, block: *mut Block) {
        let old = self.head;
        if old.is_null() {
            self.head = block;
            self.tail = block;
        } else {
            unsafe {
                (*block).allocation_list_entry().next.set(old);
                (*old).allocation_list_entry().prev.set(block);
                self.head = block;
            }
        }
    }

    pub fn push_back(&mut self, block: *mut Block) {
        let old = self.tail;

        if old.is_null() {
            self.head = block;
            self.tail = block;
        } else {
            unsafe {
                (*block).allocation_list_entry().prev.set(old);
                (*old).allocation_list_entry().next.set(block);
                self.tail = block;
            }
        }
    }

    pub fn unlink(&mut self, block: *mut Block) {
        unsafe {
            let block_entry = (*block).allocation_list_entry();
            let prev_blk = block_entry.prev.get();
            let next_blk = block_entry.next.get();

            if prev_blk.is_null() && next_blk.is_null() {
                assert_eq!(self.head, block, "invariant");
                assert_eq!(self.tail, block, "invariant");
                self.head = null_mut();
                self.tail = null_mut();
            } else if prev_blk.is_null() {
                assert_eq!(self.head, block, "invariant");

                self.head = next_blk;
                (*next_blk).allocation_list_entry().prev.set(null_mut());
            } else if next_blk.is_null() {
                assert_eq!(self.tail, block, "invariant");

                self.tail = prev_blk;
                (*prev_blk).allocation_list_entry().next.set(null_mut());
            } else {
                (*prev_blk).allocation_list_entry().next.set(next_blk);
                (*next_blk).allocation_list_entry().prev.set(prev_blk);
            }
        }
    }

    pub fn contains(&self, block: *mut Block) -> bool {
        !self.next(block).is_null() || self.tail() == block
    }
}

impl Drop for AllocationList {
    fn drop(&mut self) {
        assert!(self.head.is_null(), "deleting attached list");
        assert!(self.tail.is_null(), "deleting attached list");
    }
}

impl ActiveArray {
    pub fn new(size: usize) -> *mut Self {
        unsafe {
            let size = align_up(size, 2);
            let ptr = libc::malloc(Self::blocks_offset() + size * size_of::<*mut Block>());
            let this = ptr as *mut Self;

            std::ptr::write(
                this,
                Self {
                    size,
                    block_count: AtomicUsize::new(0),
                    refcount: AtomicIsize::new(0),
                },
            );

            this
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn block_count(&self) -> usize {
        self.block_count.load(atomic::Ordering::Relaxed)
    }

    pub fn block_count_acquire(&self) -> usize {
        self.block_count.load(atomic::Ordering::Acquire)
    }

    pub fn increment_refcount(&self) {
        let old_value = self.refcount.fetch_add(1, atomic::Ordering::Relaxed);
        assert!(old_value + 1 >= 1, "negative refcount: {}", old_value - 1);
    }

    pub fn decrement_refcount(&self) -> bool {
        let new_value = self.refcount.fetch_sub(1, atomic::Ordering::Release) - 1 ;
        assert!(new_value >= 0, "negative refcount: {}", new_value);
        new_value == 0
    }

    pub fn push(&mut self, block: *mut Block) -> bool {
        let index = self.block_count();

        if index < self.size() {
            unsafe {
                (*block).active_index = index;
                *self.block_ptr(index) = block;
                self.block_count.store(index + 1, Ordering::Release);
                true 
            }
        } else {
            false
        }
    }

    pub fn remove(&mut self, block: *mut Block) {
        assert!(self.block_count() > 0, "array is empty");
        unsafe {
            let index = (*block).active_index;
            assert_eq!(*self.block_ptr(index), block, "block not found");
            let last_index = self.block_count() - 1;
            let last_block = *self.block_ptr(last_index);
            (*last_block).active_index = index;
            *self.block_ptr(index) = last_block;
            self.block_count.store(last_index, Ordering::Relaxed);
        }
    }

    pub fn copy_from(&mut self, from: &Self) {
        let count = from.block_count();
        assert!(count <= self.size, "precondition");

        unsafe {
            let mut from_ptr = from.block_ptr(0);
            let mut to_ptr = self.block_ptr(0);

            for _ in 0..count {
                let block = from_ptr.read();
                from_ptr = from_ptr.add(1);
                *to_ptr = block;
                to_ptr = to_ptr.add(1);
            }

            self.block_count.store(count, Ordering::Relaxed);
        }
    }

    
}
