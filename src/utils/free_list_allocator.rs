use atomic::Ordering;
use std::{
    cell::UnsafeCell,
    ptr::{drop_in_place, null_mut},
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize},
};

use crate::{
    heap::atomic_load,
    sync::{lock_free_stack::{LockFreeItem, LockFreeStack}, global_counter::{GlobalCounter, CriticalSection}},
};

pub trait FreeListConfig {
    fn transfer_threshold(&self) -> usize;

    fn allocate(&self) -> *mut u8;
    fn deallocate(&self, node: *mut u8);
}

pub struct FreeNode {
    next: AtomicPtr<Self>,
}

impl FreeNode {
    pub fn next(&self) -> *mut Self {
        self.next.load(Ordering::Relaxed)
    }

    pub fn set_next(&self, next: *mut Self) {
        self.next.store(next, Ordering::Relaxed);
    }
}

impl LockFreeItem for FreeNode {
    fn next_ptr(&self) -> &AtomicPtr<Self> {
        &self.next
    }
}

type Stack = LockFreeStack<FreeNode>;

pub struct NodeList {
    head: *mut FreeNode,
    tail: *mut FreeNode,
    entry_count: usize,
}

impl NodeList {
    pub const fn new(head: *mut FreeNode, tail: *mut FreeNode, entry_count: usize) -> Self {
        Self {
            head,
            tail,
            entry_count,
        }
    }
}

pub struct PendingList {
    tail: *mut FreeNode,
    head: AtomicPtr<FreeNode>,
    count: AtomicUsize,
}

impl PendingList {
    pub const fn new() -> Self {
        Self {
            tail: std::ptr::null_mut(),
            head: AtomicPtr::new(std::ptr::null_mut()),
            count: AtomicUsize::new(0),
        }
    }

    pub unsafe fn add(&mut self, node: *mut FreeNode) -> usize {
        let old_head = self.head.swap(node, Ordering::Relaxed);

        if !old_head.is_null() {
            (*node).set_next(old_head);
        } else {
            self.tail = node;
        }

        self.count.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub unsafe fn take_all(&mut self) -> NodeList {
        let result = NodeList::new(
            self.head.load(Ordering::Relaxed),
            self.tail,
            self.count.load(Ordering::Relaxed),
        );

        self.tail = null_mut();
        self.head.store(null_mut(), Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);

        result
    }
}

pub struct FreeListAllocator<T: FreeListConfig> {
    config: T,
    free_count: AtomicUsize,
    free_list: Stack,
    transfer_lock: AtomicBool,
    active_pending_list: AtomicUsize,
    pending_lists: [UnsafeCell<PendingList>; 2],
}

impl<T: FreeListConfig> FreeListAllocator<T> {
    pub const fn new(config: T) -> Self {
        Self {
            config,
            free_count: AtomicUsize::new(0),
            free_list: Stack::new(),
            transfer_lock: AtomicBool::new(false),
            active_pending_list: AtomicUsize::new(0),
            pending_lists: [
                UnsafeCell::new(PendingList::new()),
                UnsafeCell::new(PendingList::new()),
            ],
        }
    }

    pub unsafe fn delete_list(&self, mut list: *mut FreeNode) {
        while !list.is_null() {
            let next = (*list).next();
            drop_in_place(next);
            self.config.deallocate(next.cast());
            list = next;
        }
    }

    unsafe fn get_pending_list(&self, index: usize) -> &mut PendingList {
        &mut *self.pending_lists[index].get()
    }

    pub unsafe fn reset(&mut self) {
        let index = self.active_pending_list.load(Ordering::Relaxed);
        self.get_pending_list(index).take_all();
        self.free_list.pop_all();
        self.free_count.store(0, Ordering::Relaxed);
    }

    pub fn free_count(&self) -> usize {
        self.free_count.load(Ordering::Relaxed)
    }

    pub fn pending_count(&self) -> usize {
        let index = self.active_pending_list.load(Ordering::Relaxed);
        unsafe { self.get_pending_list(index).count.load(Ordering::Relaxed) }
    }

    pub fn allocate(&self) -> *mut u8 {
        /*let mut node = null_mut();

        if self.free_count() > 0 {
            let cs = CriticalSection::new();
            node = unsafe { self.free_list.pop() };
            drop(cs);
        }

        if !node.is_null() {
            unsafe {
                drop_in_place(node);

                self.free_count.fetch_sub(1, Ordering::Relaxed);
                return node.cast();
            }
        } else {
            self.config.allocate()
        }*/

        self.config.allocate()
    }

    pub fn release(&self, node: *mut u8) {
        unsafe {
            let node = node.cast::<FreeNode>();
            
            {
                let cs = CriticalSection::new();
                let index = self.active_pending_list.load(Ordering::Acquire);
                let count = self.get_pending_list(index).add(node);
                if count <= self.config.transfer_threshold() {
                    drop(cs);
                    return;
                }
                drop(cs);
            }

            self.try_transfer_pending();
        }
    }

    pub fn try_transfer_pending(&self) -> bool {
        if self.transfer_lock.load(Ordering::Relaxed)
            || self
                .transfer_lock
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }

        let index = self.active_pending_list.load(Ordering::Relaxed);
        let new_active = (index + 1) % 2;
        self.active_pending_list.store(new_active, Ordering::Release);

        GlobalCounter::write_synchronize(|| unsafe {
            let transfer_list = self.get_pending_list(index).take_all();

            let count = transfer_list.entry_count;

            if count > 0 {
                self.free_count.fetch_add(count, Ordering::Relaxed);
                self.free_list.prepend_two(transfer_list.head, transfer_list.tail);
            }
        });

        self.transfer_lock.store(false, Ordering::Release);
        true
    }

    pub fn config(&self) -> &T {
        &self.config
    }
}

impl<T: FreeListConfig> Drop for FreeListAllocator<T> {
    fn drop(&mut self) {
        unsafe {
            let index = self.active_pending_list.load(Ordering::Relaxed);
            let pending_list = self.get_pending_list(index).take_all();
            self.delete_list(atomic_load(&pending_list.head, Ordering::Relaxed));
            self.delete_list(self.free_list.pop_all());
        }
    }
}
