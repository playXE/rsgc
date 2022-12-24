use crate::{
    system::object::{Handle, HeapObjectHeader},
    sync::{mutex::RawMutex, single_writer::SingleWriterSynchronizer},
    system::traits::Object,
    utils::log2i_graceful,
};
use atomic::{fence, Ordering};
use std::{
    cell::{Cell, UnsafeCell},
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicUsize},
};

use super::{align_down, align_up};

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
pub struct ObjStorage {
    name: String,
    active_array: *mut ActiveArray,
    allocation_list: UnsafeCell<AllocationList>,
    deferred_updates: AtomicPtr<Block>,
    allocation_mutex: RawMutex,
    active_mutex: RawMutex,

    allocation_count: AtomicUsize,
    protect_active: SingleWriterSynchronizer,
    concurrent_iteration_count: AtomicIsize,

    needs_cleanup: AtomicBool,
}

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

const SECTION_SIZE: usize = size_of::<u8>() * 8;
const SECTION_COUNT: usize = size_of::<usize>();
const BLOCK_ALIGNMENT: usize = size_of::<usize>() * SECTION_SIZE;

/// Fixed-sized array of objs, plus bookkeeping data.
/// All blocks are in the storage's _active_array, at the block's _active_index.
/// Non-full blocks are in the storage's _allocation_list, linked through the
/// block's _allocation_list_entry.  Empty blocks are at the end of that list.
#[repr(C)]
pub struct Block {
    data: [*mut HeapObjectHeader; BULK_ALLOCATE_LIMIT],
    allocated_bitmask: AtomicUsize,
    owner_address: AtomicUsize,
    memory: *mut u8,
    active_index: AtomicUsize,
    allocation_list_entry: AllocationListEntry,
    deferred_updates_next: AtomicPtr<Block>,
    release_refcount: AtomicUsize,
}

impl Drop for Block {
    fn drop(&mut self) {
        self.allocated_bitmask.store(0, Ordering::Relaxed);
        self.owner_address.store(0, Ordering::Relaxed);
    }
}

impl Block {
    pub fn new(owner: &ObjStorage, memory: *mut u8) -> Self {
        Self {
            data: [null_mut(); BULK_ALLOCATE_LIMIT],
            allocated_bitmask: AtomicUsize::new(0),
            owner_address: AtomicUsize::new(owner as *const _ as usize),
            memory,
            active_index: AtomicUsize::new(0),
            allocation_list_entry: AllocationListEntry::new(),
            deferred_updates_next: AtomicPtr::new(null_mut()),
            release_refcount: AtomicUsize::new(0),
        }
    }

    pub fn allocation_list_entry(&self) -> &AllocationListEntry {
        &self.allocation_list_entry
    }

    pub fn check_index(&self, index: usize) {
        assert!(index < self.data.len(), "Index out of bounds: {}", index);
    }

    pub fn get_pointer(&self, index: usize) -> *const *mut HeapObjectHeader {
        self.check_index(index);
        &self.data[index]
    }

    pub fn get_pointer_mut(&mut self, index: usize) -> *mut *mut HeapObjectHeader {
        self.check_index(index);
        &mut self.data[index]
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
        F: FnMut(*const *mut HeapObjectHeader) -> bool,
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

    pub fn iterate_mut<F>(&mut self, mut f: F) -> bool
    where
        F: FnMut(*mut *mut HeapObjectHeader) -> bool,
    {
        let mut bitmask = self.allocated_bitmask();
        while bitmask != 0 {
            let index = bitmask.trailing_zeros() as usize;
            let obj = self.get_pointer_mut(index);
            if !f(obj) {
                return false;
            }
            bitmask &= !(1 << index);
        }

        true
    }

    pub const fn allocation_size() -> usize {
        size_of::<Self>() + BLOCK_ALIGNMENT - size_of::<usize>()
    }

    pub const fn allocation_alignment_shift() -> usize {
        log2i_graceful(BLOCK_ALIGNMENT) as _
    }

    pub const fn is_full_bitmask(bitmask: usize) -> bool {
        !bitmask == 0
    }

    pub const fn is_empty_bitmask(bitmask: usize) -> bool {
        bitmask == 0
    }

    pub fn is_full(&self) -> bool {
        Self::is_full_bitmask(self.allocated_bitmask())
    }

    pub fn is_empty(&self) -> bool {
        Self::is_empty_bitmask(self.allocated_bitmask())
    }

    pub fn get_index(&self, ptr: *const *mut HeapObjectHeader) -> usize {
        unsafe { ptr.offset_from(self.get_pointer(0)) as usize }
    }

    pub fn bitmask_for_entry(&self, ptr: *const *mut HeapObjectHeader) -> usize {
        self.bitmask_for_index(self.get_index(ptr))
    }

    pub fn is_safe_to_delete(&self) -> bool {
        self.release_refcount.load(Ordering::Acquire) == 0
            && self.deferred_updates_next.load(Ordering::Acquire).is_null()
    }

    pub fn deferred_updates_next(&self) -> *mut Block {
        self.deferred_updates_next.load(Ordering::Acquire)
    }

    pub fn set_deferred_updates_next(&self, next: *mut Block) {
        self.deferred_updates_next.store(next, Ordering::Release);
    }

    pub fn contains(&self, ptr: *const *mut HeapObjectHeader) -> bool {
        let base = self.get_pointer(0);
        base <= ptr && (ptr < (unsafe { base.add(self.data.len()) }))
    }

    pub fn active_index(&self) -> usize {
        self.active_index.load(Ordering::Relaxed)
    }

    pub fn set_active_index(&self, index: usize) {
        self.active_index.store(index, Ordering::Relaxed);
    }

    pub fn add_allocated(&self, add: usize) {
        self.allocated_bitmask.fetch_add(add, Ordering::Relaxed);
    }

    pub fn allocate(&self) -> *const *mut HeapObjectHeader {
        let allocated = self.allocated_bitmask();
        let index = (!allocated).trailing_zeros() as usize;
        self.add_allocated(self.bitmask_for_index(index));
        self.get_pointer(index)
    }

    pub fn allocate_all(&self) -> usize {
        let new_allocated = !self.allocated_bitmask();

        self.add_allocated(new_allocated);
        new_allocated
    }

    pub fn new_block(owner: &ObjStorage) -> *mut Block {
        let size = Self::allocation_size();

        unsafe {
            let mem = libc::malloc(size).cast::<u8>();

            let block_mem = align_up(mem as _, BLOCK_ALIGNMENT);

            let block = block_mem as *mut Block;
            block.write(Block::new(owner, mem));

            block
        }
    }

    pub fn delete_block(block: *mut Block) {
        unsafe {
            let mem = (*block).memory;

            core::ptr::drop_in_place(block);

            libc::free(mem.cast());
        }
    }

    pub fn block_for_ptr(owner: &ObjStorage, ptr: *const *mut HeapObjectHeader) -> *mut Block {
        unsafe {
            let section_start = align_down(ptr as _, BLOCK_ALIGNMENT) as *mut *mut HeapObjectHeader;
            let mut section = section_start.sub(SECTION_SIZE * (SECTION_COUNT - 1));
            let owner_addr = owner as *const _ as usize;
            for _ in 0..SECTION_COUNT {
                let candidate = section.cast::<Block>();
                if (*candidate).owner_address.load(Ordering::Relaxed) == owner_addr {
                    return candidate;
                }
                section = section.add(SECTION_SIZE);
            }
            null_mut()
        }
    }

    pub fn release_entries(&mut self, releasing: usize, owner: &ObjStorage) {
        self.release_refcount.fetch_add(1, Ordering::AcqRel);

        let mut old_allocated = self.allocated_bitmask();

        loop {
            let new_value = old_allocated ^ releasing;

            match self.allocated_bitmask.compare_exchange_weak(
                old_allocated,
                new_value,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(x) => old_allocated = x,
            }
        }
        let this = self as *mut Self;

        if releasing == old_allocated || Self::is_full_bitmask(old_allocated) {
            if self.deferred_updates_next().is_null() {
                self.deferred_updates_next.store(this as *mut Self, Ordering::Release);

                let mut head = owner.deferred_updates.load(Ordering::Acquire);
                loop {
                    self.deferred_updates_next.store(if head.is_null() {
                        this as *mut Self 
                    } else {
                        head 
                    }, Ordering::Relaxed);
                    match owner.deferred_updates.compare_exchange_weak(head, self as *mut Self, Ordering::AcqRel, Ordering::Relaxed) {
                        Ok(_) => break,
                        Err(v) => head = v 
                    }
                }

                if releasing == old_allocated {
                    // todo 
                }
            }
        }
        self.release_refcount.fetch_sub(1, Ordering::AcqRel);
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
    pub fn blocks_offset() -> usize {
        crate::utils::round_up(size_of::<Self>(), size_of::<*mut Block>(), 0)
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
            let ptr = libc::malloc(Self::blocks_offset() + size * size_of::<*mut Block>());
            println!("alloc {:p} {}", ptr, size * size_of::<*mut Block>());
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

    pub fn destroy(this: *mut Self) {
        unsafe {
            core::ptr::drop_in_place(this);
            let ptr = this as *const Self as *mut u8;
            libc::free(ptr as _);
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
        let new_value = self.refcount.fetch_sub(1, atomic::Ordering::Release) - 1;
        assert!(new_value >= 0, "negative refcount: {}", new_value);
        new_value == 0
    }

    pub fn push(&mut self, block: *mut Block) -> bool {
        let index = self.block_count();

        if index < self.size() {
            unsafe {
                (*block).set_active_index(index);
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
            let index = (*block).active_index();
            assert_eq!(*self.block_ptr(index), block, "block not found");
            let last_index = self.block_count() - 1;
            let last_block = *self.block_ptr(last_index);
            (*last_block).set_active_index(index);
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

impl ObjStorage {
    pub fn allocate(&self) -> *mut *mut HeapObjectHeader {
        unsafe {
            self.allocation_mutex.lock(false);
            let block = self.block_for_allocation();
            let result = (*block).allocate();
            self.allocation_count.fetch_add(1, Ordering::Relaxed);
            if (*block).is_full() {
                self.allocation_list().unlink(block);
            }
            self.allocation_mutex.unlock();
            result as _
        }
    }

    pub fn allocaten(&self, ptrs: &mut [*mut *mut HeapObjectHeader]) -> usize {
        let block;
        let mut taken;
        let size = ptrs.len();
        unsafe {
            self.allocation_mutex.lock(false);
            block = self.block_for_allocation();
            if block.is_null() {
                return 0;
            }
            self.allocation_list().unlink(block);
            taken = (*block).allocate_all();
            self.allocation_mutex.unlock();

            let num_taken = taken.count_ones() as usize;
            self.allocation_count.fetch_add(num_taken, Ordering::Relaxed);

            let limit = num_taken.min(size);

            for i in 0..limit {
                let index = taken.trailing_zeros() as usize;
                taken ^= (*block).bitmask_for_index(index);
                ptrs[i] = (*block).get_pointer_mut(index);
            }

            if taken == 0 {
                assert!(num_taken == limit);
            } else {
                (*block).release_entries(taken, self);
                self.allocation_count.fetch_sub(num_taken - limit, Ordering::Relaxed);
            }

            limit
        }
    }

    unsafe fn allocation_list(&self) -> &mut AllocationList {
        &mut *self.allocation_list.get()
    }

    unsafe fn block_for_allocation(&self) -> *mut Block {
        assert!(self.allocation_mutex.is_locked());

        loop {
            let block = self.allocation_list().head();
            if !block.is_null() {
                return block;
            } else if self.reduce_deferred_updates() {
                // Might have added a block to the _allocation_list, so retry.
            } else if self.try_add_block() {
                assert!(!self.allocation_list().head().is_null());
            } else if !self.allocation_list().head().is_null() {
                // Trying to add a block failed, but some other thread added to the
                // list while we'd dropped the lock over the new block allocation.
            } else if !self.reduce_deferred_updates() {
                // Once more before failure.
                // Attempt to add a block failed, no other thread added a block,
                // and no deferred updated added a block, then allocation failed.
                return null_mut();
            }
        }
    }

    unsafe fn try_add_block(&self) -> bool {
        assert!(self.allocation_mutex.is_locked());

        let block;
        {
            self.allocation_mutex.unlock();
            block = Block::new_block(self);
            self.allocation_mutex.lock(false);
        }

        if !(*self.active_array).push(block) {
            if self.expand_active_array() {
                assert!(
                    (*self.active_array).push(block),
                    "push failed after expansion"
                );
            } else {
                log::debug!(target: "objstorage", "{}: failed active array expand", self.name);
                Block::delete_block(block);
                return false;
            }
        }
        self.allocation_list().push_back(block);
        log::debug!(target: "objstorage", "{}: new block  {:p}", self.name, block);
        true
    }

    pub(crate) unsafe fn expand_active_array(&self) -> bool {
        assert!(self.allocation_mutex.is_locked());
        let old_array = self.active_array;
        let new_size = (*old_array).size() * 2;
        let new_array = ActiveArray::new(new_size);
        (*new_array).copy_from(&*old_array);

        self.replace_active_array(new_array);
        self.relinquish_block_array(old_array);
        true
    }

    pub(crate) unsafe fn replace_active_array(&self, new_array: *mut ActiveArray) {
        // Caller has the old array that is the current value of _active_array.
        // Update new_array refcount to account for the new reference.
        (*new_array).increment_refcount();
        // Install new_array, ensuring its initialization is complete first.
        super::atomic_store(&self.active_array, new_array, Ordering::Release);
        // Wait for any readers that could read the old array from _active_array.
        self.protect_active.synchronize();
        // All obtain critical sections that could see the old array have
        // completed, having incremented the refcount of the old array.  The
        // caller can now safely relinquish the old array.
    }

    pub(crate) unsafe fn relinquish_block_array(&self, array: *mut ActiveArray) {
        if (*array).decrement_refcount() {
            ActiveArray::destroy(array);
        }
    }

    unsafe fn reduce_deferred_updates(&self) -> bool {
        assert!(self.allocation_mutex.is_locked());

        let mut block = self.deferred_updates.load(Ordering::Acquire);
        loop {
            if block.is_null() {
                return false;
            }

            let mut tail = (*block).deferred_updates_next();

            if block == tail {
                tail = null_mut();
            }

            match self.deferred_updates.compare_exchange(
                block,
                tail,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(fetched) => block = fetched,
            }
        }

        (*block).set_deferred_updates_next(null_mut());

        fence(Ordering::SeqCst);

        let allocated = (*block).allocated_bitmask();

        if Block::is_full_bitmask(allocated) {
            assert!(!self.allocation_list().contains(block), "invariant");
        } else if self.allocation_list().contains(block) {
            if Block::is_empty_bitmask(allocated) {
                self.allocation_list().unlink(block);
                self.allocation_list().push_back(block);
            }
        } else if Block::is_empty_bitmask(allocated) {
            // Block is empty and not in list. Add to back for possible deletion.
            self.allocation_list().push_back(block);
        } else {
            // Block is neither full nor empty, and not in list.  Add to front.
            self.allocation_list().push_front(block);
        }

        true
    }

    pub fn release(&self, ptr: *mut *mut HeapObjectHeader) {
        let block = Block::block_for_ptr(self, ptr);

        assert!(!block.is_null(), "invalid release {}: {:p}", self.name, ptr);

        unsafe {
            (*block).release_entries((*block).bitmask_for_entry(ptr), self);
            self.allocation_count.fetch_sub(1, Ordering::AcqRel);
        }
    }
    
    pub fn releasen(&self, ptrs: &[*mut *mut HeapObjectHeader]) {
        let mut i = 0;
        while i < ptrs.len() {
            self.release(ptrs[i]);
            i += 1;
        }
    }

    pub fn new(name: &str) -> Self {
        let name = name.to_owned();
        let active_array = ActiveArray::new(8);
        let allocation_list = UnsafeCell::new(AllocationList::new());
        let allocation_mutex = RawMutex::new();
        let allocation_count = AtomicUsize::new(0);
        let deferred_updates = AtomicPtr::new(null_mut());
        let protect_active = SingleWriterSynchronizer::new();

        let this = Self {
            name,
            active_array,
            allocation_list,
            allocation_mutex,
            allocation_count,
            deferred_updates,
            protect_active,
            active_mutex: RawMutex::new(),
            concurrent_iteration_count: AtomicIsize::new(0),
            needs_cleanup: AtomicBool::new(false)
        };
        unsafe {
            (*this.active_array).increment_refcount();
        }
        this 
    }
}

impl Drop for ObjStorage {
    fn drop(&mut self) {
        let mut block;
        unsafe {
            block = self.deferred_updates.load(Ordering::Relaxed);

            while !block.is_null() {
                self.deferred_updates.store((*block).deferred_updates_next(), Ordering::Relaxed);
                (*block).set_deferred_updates_next(null_mut());
                block = self.deferred_updates.load(Ordering::Relaxed);
            }

            while !self.allocation_list().head().is_null() {
                block = self.allocation_list().head();
                
                self.allocation_list().unlink(block);
            }

            let unreferenced = (*self.active_array).decrement_refcount();

            assert!(unreferenced);

            let mut i = (*self.active_array).block_count();

            while 0 < i {
                i -= 1;
                let block = (*self.active_array).at(i);
                Block::delete_block(block);
            }

            ActiveArray::destroy(self.active_array);
        }
    }
}