use std::{mem::size_of, ptr::null_mut, sync::atomic::{AtomicPtr, AtomicUsize}};

use crate::sync::monitor::Monitor;

pub struct LocalSSB {
    pub index: usize,
    pub buf: *mut *mut u8
}

impl LocalSSB {
    pub fn try_enqueue(&mut self, obj: *mut u8) -> bool {
        let mut index = self.index;
        if index == 0 {
            return false;
        }

        let buffer = self.buf;
        unsafe {
            index -= 1;
            *buffer.add(index) = obj;
            self.index = index;
        }
        true
    }

    pub fn set_index(&mut self, index: usize) {
        self.index = index;
    }

    pub fn set_buffer(&mut self, buf: *mut *mut u8) {
        self.buf = buf;
    }

    pub fn buffer(&self) -> *mut *mut u8 {
        self.buf 
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn new() -> Self {
        Self {
            index: 0,
            buf: null_mut()
        }
    }
}

