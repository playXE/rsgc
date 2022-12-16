//! There are various techniques that require threads to be able to log
//! addresses.  For example, a generational write barrier might log
//! the addresses of modified old-generation objects.  This type supports
//! this operation.

use std::{
    mem::size_of,
    ptr::{drop_in_place, null_mut},
    sync::atomic::{AtomicPtr, AtomicUsize},
};

use crate::sync::lock_free_stack::LockFreeItem;

use super::free_list_allocator::{FreeListAllocator, FreeListConfig};

#[repr(C)]
pub struct PtrQueue {
    // The (byte) index at which an object was last enqueued.  Starts at
    // capacity (in bytes) (indicating an empty buffer) and goes towards zero.
    // Value is always pointer-size aligned.
    pub index: usize,
    // Size of the current buffer, in bytes.
    // Value is always pointer-size aligned.
    pub capacity_in_bytes: usize,

    pub buf: *mut *mut u8,
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

struct BufferAllocatorConfig {
    buffer_size: usize,
    threshold: usize,
}

impl FreeListConfig for BufferAllocatorConfig {
    fn allocate(&self) -> *mut u8 {
        unsafe {
            let byte_size = size_of::<usize>() * self.buffer_size;
            libc::malloc(BufferNode::buffer_offset() + byte_size).cast()
        }
    }

    fn deallocate(&self, node: *mut u8) {
        unsafe {
            libc::free(node.cast());
        }
    }

    fn transfer_threshold(&self) -> usize {
        10
    }
}

pub struct Allocator {
    free_list: FreeListAllocator<BufferAllocatorConfig>,
}

impl Allocator {
    pub fn new(buffer_size: usize) -> Self {
        Self {
            free_list: FreeListAllocator::new(BufferAllocatorConfig {
                buffer_size,
                threshold: 10,
            }),
        }
    }

    pub fn buffer_size(&self) -> usize {
        self.free_list.config().buffer_size
    }

    pub fn free_count(&self) -> usize {
        self.free_list.free_count()
    }

    pub fn allocate(&self) -> *mut BufferNode {
        let node = self.free_list.allocate();
        BufferNode::new(node)
    }

    pub fn release(&self, node: *mut BufferNode) {
        unsafe {
            drop_in_place(node);
            self.free_list.release(node.cast());
        }
    }
}

pub trait PtrQueueImpl {
    fn create<T: PtrQueueSetImpl>(qset: &T) -> Self;

    fn set_buffer(&mut self, buf: *mut *mut u8);
    fn set_index(&mut self, index: usize);
    fn index(&self) -> usize;
    fn buffer(&self) -> *mut *mut u8;
}

impl PtrQueueImpl for PtrQueue {
    fn create<T: PtrQueueSetImpl>(qset: &T) -> Self {
        Self {
            index: 0,
            buf: null_mut(),
            capacity_in_bytes: Self::index_to_byte_index(qset.buffer_size()),
        }
    }

    fn set_buffer(&mut self, buf: *mut *mut u8) {
        self.buf = buf;
    }

    fn buffer(&self) -> *mut *mut u8 {
        self.buf
    }

    fn index(&self) -> usize {
        self.index
    }

    fn set_index(&mut self, index: usize) {
        self.index = index;
    }
}

pub trait PtrQueueSetImpl {
    type Queue: PtrQueueImpl;

    fn allocator(&self) -> &Allocator;

    fn new(allocator: Allocator) -> Self;

    fn reset_queue(&self, queue: &mut Self::Queue) {
        if !queue.buffer().is_null() {
            queue.set_index(self.allocator().buffer_size());
        }
    }

    fn flush_queue(&self, queue: &mut Self::Queue) {
        let buffer = queue.buffer();
        if !buffer.is_null() {
            let index = queue.index();
            queue.set_buffer(null_mut());
            queue.set_index(0);

            unsafe {
                let node = BufferNode::make_node_from_buffer(buffer, index);

                if index == self.allocator().buffer_size() {
                    self.deallocate_buffer(node);
                } else {
                    self.enqueue_completed_buffer(node);
                }
            }
        }
    }

    #[inline]
    fn try_enqueue(&self, queue: &mut Self::Queue, value: *mut u8) -> bool {
        let mut index = queue.index();
        if index == 0 {
            return false;
        }

        let buffer = queue.buffer();
        unsafe {
            index -= 1;
            buffer.add(index).write(value);
            queue.set_index(index);
        }
        true
    }

    fn retry_enqueue(&self, queue: &mut Self::Queue, value: *mut u8) {
        assert!(queue.index() != 0);
        assert!(!queue.buffer().is_null());

        unsafe {
            let mut index = queue.index();
            index -= 1;
            queue.buffer().add(index).write(value);
            queue.set_index(index);
        }
    }

    fn exchange_buffer_with_new(&self, queue: &mut Self::Queue) -> *mut BufferNode {
        let mut node = null_mut();
        let buffer = queue.buffer();

        if !buffer.is_null() {
            node = unsafe { BufferNode::make_node_from_buffer(buffer, queue.index()) };
        }

        self.install_new_buffer(queue);
        node
    }

    fn install_new_buffer(&self, queue: &mut Self::Queue) {
        queue.set_buffer(self.allocate_buffer());
        queue.set_index(self.allocator().buffer_size());
    }

    fn allocate_buffer(&self) -> *mut *mut u8 {
        unsafe { BufferNode::make_buffer_from_node(self.allocator().allocate()) }
    }

    fn deallocate_buffer(&self, node: *mut BufferNode) {
        self.allocator().release(node.cast());
    }

    fn enqueue_completed_buffer(&self, node: *mut BufferNode);

    fn buffer_size(&self) -> usize {
        self.allocator().buffer_size()
    }
}
