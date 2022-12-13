//! There are various techniques that require threads to be able to log
//! addresses.  For example, a generational write barrier might log
//! the addresses of modified old-generation objects.  This type supports
//! this operation.

use std::{mem::size_of, sync::atomic::AtomicPtr};

use crate::sync::lock_free_stack::LockFreeItem;

pub struct PtrQueue {
    // The (byte) index at which an object was last enqueued.  Starts at
    // capacity (in bytes) (indicating an empty buffer) and goes towards zero.
    // Value is always pointer-size aligned.
    index: usize,
    // Size of the current buffer, in bytes.
    // Value is always pointer-size aligned.
    capacity_in_bytes: usize,

    pub(crate) buf: *mut *mut u8,
}

impl PtrQueue {
    pub fn capacity_in_bytes(&self) -> usize {
        self.capacity_in_bytes
    }

    pub fn byte_index_to_index(ind: usize) -> usize {
        ind / size_of::<usize>()
    }

    pub fn index_to_byte_index(ind: usize) -> usize {
        ind * size_of::<usize>()
    }

    pub fn buffer(&self) -> *mut *mut u8 {
        self.buf
    }

    pub fn set_buffer(&mut self, buf: *mut *mut u8) {
        self.buf = buf;
    }

    pub fn set_index(&mut self, ind: usize) {
        self.index = ind;
    }

    pub fn capacity(&self) -> usize {
        Self::byte_index_to_index(self.capacity_in_bytes())
    }
}

#[repr(C)]
pub struct BufferNode {
    index: usize,
    next: AtomicPtr<Self>,
    buffer: [*mut u8; 1], // pseudo flexible array member
}

impl LockFreeItem for BufferNode {
    fn next_ptr(&self) -> &AtomicPtr<Self> {
        &self.next
    }
}

impl BufferNode {
    pub const fn buffer_offset() -> usize {
        memoffset::offset_of!(Self, buffer)
    }

    pub fn new(at: *mut u8) -> *mut Self {
        let node = at as *mut Self;
        unsafe {
            (*node).index = 0;
            (*node).next = AtomicPtr::new(core::ptr::null_mut());
        }
        node
    }
    pub fn next(&self) -> *mut Self {
        self.next.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_next(&self, next: *mut Self) {
        self.next.store(next, core::sync::atomic::Ordering::Relaxed)
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn set_index(&mut self, ind: usize) {
        self.index = ind;
    }

    pub unsafe fn make_node_from_buffer(buffer: *mut *mut u8, index: usize) -> *mut Self {
        let node = buffer.sub(Self::buffer_offset()) as *mut Self;
        (*node).index = index;
        (*node).next = AtomicPtr::new(core::ptr::null_mut());
        node
    }

    pub unsafe fn make_buffer_from_node(node: *mut Self) -> *mut *mut u8 {
        node.cast::<u8>().add(Self::buffer_offset()) as *mut *mut u8
    }
}
