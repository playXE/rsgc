use crate::{
    base::monitor::{Monitor, MonitorLock},
    memory::object_header::ObjectHeader,
};
use std::sync::atomic;
use std::{ptr::null_mut, sync::atomic::AtomicUsize};

use super::visitor::Visitor;

pub struct PointerBlock<const SIZE: usize> {
    next: *mut Self,
    top: i32,
    pointers: [*mut ObjectHeader; SIZE],
}

impl<const SIZE: usize> PointerBlock<SIZE> {
    pub fn new() -> *mut Self {
        Box::into_raw(Box::new(Self {
            next: null_mut(),
            top: 0,
            pointers: [null_mut(); SIZE],
        }))
    }
    pub fn reset(&mut self) {
        self.top = 0;
        self.next = null_mut();
    }

    pub fn next(&self) -> *mut Self {
        self.next
    }

    pub fn set_next(&mut self, next: *mut Self) {
        self.next = next;
    }

    pub fn count(&self) -> usize {
        self.top as usize
    }
    pub fn is_full(&self) -> bool {
        self.top == SIZE as i32
    }
}

pub struct BlockStack<const BLOCK_SIZE: usize> {
    monitor: Monitor<(BlockStackList<BLOCK_SIZE>, BlockStackList<BLOCK_SIZE>)>,
}

impl<const BLOCK_SIZE: usize> BlockStack<BLOCK_SIZE> {}

pub struct BlockStackList<const BLOCK_SIZE: usize> {
    head: *mut PointerBlock<BLOCK_SIZE>,
    length: usize,
}

impl<const BLOCK_SIZE: usize> BlockStackList<BLOCK_SIZE> {
    pub fn new() -> Self {
        Self {
            head: null_mut(),
            length: 0,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.head.is_null()
    }
    pub fn pop(&mut self) -> *mut PointerBlock<BLOCK_SIZE> {
        unsafe {
            let result = self.head;
            self.head = (*result).next;
            self.length -= 1;
            (*result).next = null_mut();
            result
        }
    }

    pub fn push(&mut self, block: *mut PointerBlock<BLOCK_SIZE>) {
        unsafe {
            (*block).next = self.head;
            self.head = block;
            self.length += 1;
        }
    }

    pub fn pop_all(&mut self) -> *mut PointerBlock<BLOCK_SIZE> {
        let result = self.head;
        self.head = null_mut();
        self.length = 0;
        result
    }
}

impl<const BLOCK_SIZE: usize> Drop for BlockStackList<BLOCK_SIZE> {
    fn drop(&mut self) {
        unsafe {
            while !self.is_empty() {
                let ptr = self.pop();

                let _ = Box::from_raw(ptr);
            }
        }
    }
}

impl<const BLOCK_SIZE: usize> BlockStack<BLOCK_SIZE> {
    pub fn new() -> Self {
        Self {
            monitor: Monitor::new((BlockStackList::new(), BlockStackList::new())),
        }
    }

    pub fn reset(&self) {
        let mut lock = MonitorLock::new(&self.monitor);
        unsafe {
            while !lock.0.is_empty() {
                let block = lock.0.pop();
                (*block).reset();
                let _ = Box::from_raw(block);
            }
            while !lock.1.is_empty() {
                let block = lock.1.pop();
                (*block).reset();
                let _ = Box::from_raw(block);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        let ml = MonitorLock::new(&self.monitor);
        ml.0.is_empty() && ml.1.is_empty()
    }

    fn is_empty_locked(&self) -> bool {
        unsafe {
            let data = self.monitor.data();
            data.0.is_empty() && data.1.is_empty()
        }
    }

    pub fn take_blocks(&self) -> *mut PointerBlock<BLOCK_SIZE> {
        let mut lock = MonitorLock::new(&self.monitor);

        while !lock.1.is_empty() {
            let p = lock.1.pop();
            lock.0.push(p);
        }
        lock.0.pop_all()
    }

    fn push_block_impl(&self, block: *mut PointerBlock<BLOCK_SIZE>) {
        unsafe {
            if (*block).is_full() {
                let mut ml = MonitorLock::new(&self.monitor);
                let was_empty = self.is_empty_locked();
                ml.0.push(block);
                if was_empty {
                    ml.notify_one();
                }
            } else if (*block).is_empty() {
                let _ = Box::from_raw(block);
            } else {
                let mut ml = MonitorLock::new(&self.monitor);
                let was_empty = self.is_empty_locked();
                ml.1.push(block);
                if was_empty {
                    ml.notify_one();
                }
            }
        }
    }

    pub fn wait_for_work(&self, num_busy: &AtomicUsize) -> *mut PointerBlock<BLOCK_SIZE> {
        let mut ml = MonitorLock::new(&self.monitor);

        if num_busy.fetch_sub(1, atomic::Ordering::Relaxed) == 1 {
            // This is the last worker, wake the others now that we know no further work
            // will come.
            ml.notify_all();
            return null_mut();
        }

        loop {
            if !ml.0.is_empty() {
                num_busy.fetch_add(1, atomic::Ordering::Relaxed);
                return ml.0.pop();
            }

            if !ml.1.is_empty() {
                num_busy.fetch_add(1, atomic::Ordering::Relaxed);
                return ml.1.pop();
            }

            ml.wait();

            if num_busy.load(atomic::Ordering::Relaxed) == 0 {
                return null_mut();
            }
        }
    }

    pub fn pop_non_full_block(&self) -> *mut PointerBlock<BLOCK_SIZE> {
        let mut ml = MonitorLock::new(&self.monitor);

        if !ml.1.is_empty() {
            return ml.1.pop();
        }

        self.pop_empty_block()
    }

    pub fn pop_empty_block(&self) -> *mut PointerBlock<BLOCK_SIZE> {
        PointerBlock::new()
    }

    pub fn pop_non_empty_block(&self) -> *mut PointerBlock<BLOCK_SIZE> {
        let mut ml = MonitorLock::new(&self.monitor);

        if !ml.0.is_empty() {
            return ml.0.pop();
        } else if !ml.1.is_empty() {
            return ml.1.pop();
        } else {
            null_mut()
        }
    }
}

pub trait Block {
    fn push(&mut self, obj: *mut ObjectHeader);
    fn pop(&mut self) -> *mut ObjectHeader;

    fn is_empty(&self) -> bool;

    unsafe fn visit_object_pointers(&mut self, visitor: &mut dyn Visitor);
}

impl<const SIZE: usize> Block for PointerBlock<SIZE> {
    fn push(&mut self, pointer: *mut ObjectHeader) {
        assert!(!self.is_full());
        self.pointers[self.top as usize] = pointer;
        self.top += 1;
    }

    fn pop(&mut self) -> *mut ObjectHeader {
        assert!(!self.is_empty());
        self.top -= 1;
        self.pointers[self.top as usize]
    }

    unsafe fn visit_object_pointers(&mut self, visitor: &mut dyn Visitor) {
        visitor.visit_pointers_len(&mut self.pointers[0], self.top as _);
    }

    fn is_empty(&self) -> bool {
        self.top == 0
    }
}

pub trait Stack {
    type Block: Block;

    fn push_block(&self, block: *mut Self::Block);
    fn pop_empty_block(&self) -> *mut Self::Block;
    fn pop_non_empty_block(&self) -> *mut Self::Block;
    fn pop_non_full_block(&self) -> *mut Self::Block;

    fn is_empty(&self) -> bool;

    fn wait_for_work(&self, num_busy: &AtomicUsize) -> *mut Self::Block;
}

pub type StoreBuffer = BlockStack<1024>;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThresholdPolicy {
    CheckTreshold,
    IgnoreTreshold,
}

impl StoreBuffer {
    pub fn overflowd(&self) -> bool {
        let ml = MonitorLock::new(&self.monitor);
        ml.0.length + ml.1.length > 100
    }
    pub fn push_block(&self, block: *mut PointerBlock<1024>, policy: ThresholdPolicy) -> bool {
        self.push_block_impl(block);

        if policy == ThresholdPolicy::CheckTreshold && self.overflowd() {
            true
        } else {
            false
        }
    }
}

pub type MarkingStack = BlockStack<64>;

impl MarkingStack {
    pub fn push_block(&self, block: *mut PointerBlock<64>) {
        self.push_block_impl(block);
    }
}

pub type PromotionStack = MarkingStack;

pub struct BlockWorkList {
    local_input: *mut PointerBlock<64>,
    local_output: *mut PointerBlock<64>,
    stack: *const BlockStack<64>,
}

impl BlockWorkList {
    pub fn new(stack: *const BlockStack<64>) -> Self {
        unsafe {
            Self {
                local_input: (*stack).pop_empty_block(),
                local_output: (*stack).pop_empty_block(),
                stack,
            }
        }
    }

    pub fn pop(&mut self) -> *mut ObjectHeader {
        unsafe {
            if (*self.local_input).is_empty() {
                if !(*self.local_output).is_empty() {
                    let temp = self.local_output;
                    self.local_output = self.local_input;
                    self.local_input = temp;
                } else {
                    let new_work = (*self.stack).pop_non_empty_block();

                    if new_work.is_null() {
                        return null_mut();
                    }

                    (*self.stack).push_block(self.local_input);
                    self.local_input = new_work;
                }
            }
            (*self.local_input).pop()
        }
    }

    pub fn push(&mut self, object: *mut ObjectHeader) {
        unsafe {
            if (*self.local_output).is_full() {
                (*self.stack).push_block(self.local_output);
                self.local_output = (*self.stack).pop_empty_block();
            }

            (*self.local_output).push(object);
        }
    }

    pub fn flush(&mut self) {
        unsafe {
            if !(*self.local_output).is_empty() {
                (*self.stack).push_block(self.local_output);
                self.local_output = (*self.stack).pop_empty_block();
            }

            if !(*self.local_input).is_empty() {
                (*self.stack).push_block(self.local_input);
                self.local_input = (*self.stack).pop_empty_block();
            }
        }
    }

    pub fn wait_for_work(&mut self, num_busy: &AtomicUsize) -> bool {
        unsafe {
            let new_work = (*self.stack).wait_for_work(num_busy);

            if new_work.is_null() {
                return false;
            }

            (*self.stack).push_block(self.local_input);
            self.local_input = new_work;
            true
        }
    }

    pub fn finalize(&mut self) {
        unsafe {
            (*self.stack).push_block(self.local_output);
            self.local_output = null_mut();
            (*self.stack).push_block(self.local_input);
            self.local_input = null_mut();

            self.stack = null_mut();
        }
    }

    pub fn abandon_work(&mut self) {
        unsafe {
            (*self.stack).push_block(self.local_output);
            self.local_output = null_mut();
            (*self.stack).push_block(self.local_input);
            self.local_input = null_mut();
        }
    }

    pub fn is_empty(&self) -> bool {
        unsafe {
            if !(*self.local_input).is_empty() {
                return false;
            }

            if !(*self.local_output).is_empty() {
                return false;
            }

            (*self.stack).is_empty()
        }
    }
}

pub type MarkerWorkList = BlockWorkList;
pub type PromotionWorkList = BlockWorkList;
