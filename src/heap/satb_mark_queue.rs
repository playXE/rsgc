use std::{
    ops::{Deref, DerefMut},
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicUsize},
};

use atomic::Ordering;

use crate::{
    heap::thread::threads,
    object::HeapObjectHeader,
    offsetof,
    sync::{
        global_counter::{CriticalSection, GlobalCounter},
        lock_free_stack::LockFreeStack,
    },
    utils::ptr_queue::{Allocator, BufferNode, PtrQueue, PtrQueueImpl, PtrQueueSetImpl},
};

use super::{heap::heap, safepoint::SafepointSynchronize};

#[repr(C)]
pub struct SATBMarkQueue {
    pub queue: PtrQueue,
    pub is_active: bool,
}

impl Deref for SATBMarkQueue {
    type Target = PtrQueue;
    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

impl DerefMut for SATBMarkQueue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.queue
    }
}

impl SATBMarkQueue {
    pub const fn is_active(&self) -> bool {
        self.is_active
    }

    pub fn set_active(&mut self, value: bool) {
        self.is_active = value;
    }

    pub fn byte_offset_of_index() -> usize {
        offsetof!(Self, queue.index)
    }

    pub fn byte_offset_of_buf() -> usize {
        offsetof!(Self, queue.buf)
    }

    pub fn byte_offset_of_active() -> usize {
        offsetof!(Self, is_active)
    }
}

impl PtrQueueImpl for SATBMarkQueue {
    fn buffer(&self) -> *mut *mut u8 {
        self.queue.buf
    }

    fn create<T: crate::utils::ptr_queue::PtrQueueSetImpl>(qset: &T) -> Self {
        Self {
            is_active: false,
            queue: PtrQueue::create(qset),
        }
    }

    fn index(&self) -> usize {
        self.queue.index()
    }

    fn set_buffer(&mut self, buf: *mut *mut u8) {
        self.queue.set_buffer(buf)
    }

    fn set_index(&mut self, index: usize) {
        self.queue.set_index(index)
    }
}

use parking_lot::{lock_api::RawMutex, RawMutex as Lock};

pub struct SATBMarkQueueSet {
    allocator: Allocator,
    list: LockFreeStack<BufferNode>,
    count_and_process_flag: AtomicUsize,
    process_completed_buffers_threshold: AtomicUsize,
    buffer_enqueue_threshold: AtomicUsize,
    all_active: AtomicBool,
    lock: Lock,
}

impl SATBMarkQueueSet {
    pub fn new(buffer_size: usize) -> Self {
        Self {
            lock: Lock::INIT,
            allocator: Allocator::new(buffer_size),
            list: LockFreeStack::new(),
            count_and_process_flag: AtomicUsize::new(0),
            process_completed_buffers_threshold: AtomicUsize::new(usize::MAX),
            buffer_enqueue_threshold: AtomicUsize::new(0),
            all_active: AtomicBool::new(false),
        }
    }

    pub fn buffer_enqueue_threshold(&self) -> usize {
        self.buffer_enqueue_threshold.load(Ordering::Relaxed)
    }

    pub fn is_active(&self) -> bool {
        self.all_active.load(Ordering::Relaxed)
    }

    pub fn filter(&self, filter_out: impl Fn(*mut u8) -> bool, queue: &mut SATBMarkQueue) {
        let buf = queue.buffer();

        if buf.is_null() {
            return;
        }

        unsafe {
            // Two-fingered compaction toward the end.
            let mut src = buf.add(queue.index());
            let mut dst = buf.add(self.buffer_size());

            while src < dst {
                // Search low to high for an entry to keep.
                let entry = src.read();
                if !filter_out(entry) {
                    // Found keeper.  Search high to low for an entry to discard.
                    while {
                        dst = dst.sub(1);
                        src < dst
                    } {
                        if filter_out(dst.read()) {
                            *dst = entry; // Replace discard with keeper.
                            break;
                        }
                    }

                    // If discard search failed (src == dst), the outer loop will also end.
                }
                src = src.add(1);
            }

            // dst points to the lowest retained entry, or the end of the buffer
            // if all the entries were filtered out.
            queue.set_index(dst.offset_from(buf) as usize);
        }
    }

    /// The number of buffers in the list.  Racy and not updated atomically
    /// with the set of completed buffers.
    pub fn completed_buffers_num(&self) -> usize {
        self.count_and_process_flag.load(atomic::Ordering::Relaxed) >> 1
    }

    /// Return true if completed buffers should be processed.
    pub fn process_completed_buffers(&self) -> bool {
        (self.count_and_process_flag.load(atomic::Ordering::Relaxed) & 1) != 0
    }

    pub fn set_active_all_threads(&self, active: bool, _expected_active: bool) {
        assert!(
            SafepointSynchronize::is_at_safepoint(),
            "Must be at safepoint"
        );

        self.all_active.store(false, atomic::Ordering::Release);

        let threads = threads();

        threads
            .get()
            .threads
            .iter()
            .copied()
            .for_each(|thread| unsafe {
                let queue = (*thread).satb_mark_queue_mut();
                if !queue.buffer().is_null() {
                    assert!(
                        !active || queue.index() == self.buffer_size(),
                        "queues should be empty when activated"
                    );
                    queue.set_index(self.buffer_size());
                }
                (*thread).satb_mark_queue_mut().set_active(active);
            });
    }

    pub fn filter_marked(&self, queue: &mut SATBMarkQueue) {
        let heap = heap();
        self.filter(|obj| !heap.marking_context().is_marked(obj.cast()), queue);
    }

    pub fn flush_queue(&self, queue: &mut SATBMarkQueue) {
        // Filter now to possibly save work later.  If filtering empties the
        // buffer then flush_queue can deallocate the buffer.
        self.filter_marked(queue);

        let buffer = queue.buffer();
        if !buffer.is_null() {
            let index = queue.index();
            queue.set_buffer(null_mut());
            queue.set_index(0);

            unsafe {
                let node = BufferNode::make_node_from_buffer(buffer, index);
                println!("flush queue: {}, {}", index, self.allocator().buffer_size());
                if index == self.allocator().buffer_size() {
                    self.lock.lock();
                    self.deallocate_buffer(node);

                    self.lock.unlock();
                } else {
                    self.enqueue_completed_buffer(node);
                }
            }
        }
    }

    pub fn get_completed_buffer(&self) -> *mut BufferNode {
        let node;
        unsafe {
            self.lock.lock();
            node = self.list.pop();
        }

        if !node.is_null() {
            decrement_count(&self.count_and_process_flag);
        }
        unsafe {
            self.lock.unlock();
        }

        node
    }

    pub fn should_enqueue_buffer(&self, queue: &SATBMarkQueue) -> bool {
        queue.index() < self.buffer_enqueue_threshold()
    }

    #[inline(never)]
    #[cold]
    pub fn handle_zero_index(&self, queue: &mut SATBMarkQueue) {
        if queue.buffer().is_null() {
            self.lock.lock();
            self.install_new_buffer(queue);
            unsafe {
                self.lock.unlock();
            }
        } else {
            self.filter_marked(queue);
            if self.should_enqueue_buffer(queue) {
                self.enqueue_completed_buffer(self.exchange_buffer_with_new(queue));
            }
        }
        assert!(!queue.buffer().is_null());
        assert!(queue.index() != 0);
    }

    pub fn apply_closure_to_completed_buffer<F: FnOnce(*mut *mut u8, usize)>(&self, cl: F) -> bool {
        let nd = self.get_completed_buffer();

        if !nd.is_null() {
            unsafe {
                let buffer = BufferNode::make_buffer_from_node(nd);
                let index = (*nd).index();
                let size = self.buffer_size();
                assert!(index <= size);

                cl(buffer.add(index), size - index);
                self.deallocate_buffer(nd);
                true
            }
        } else {
            false
        }
    }
    #[inline]
    pub fn enqueue_known_active(&self, queue: &mut SATBMarkQueue, obj: *mut HeapObjectHeader) {
        if !self.try_enqueue(queue, obj.cast()) {
            self.enqueue_known_active_slow(queue, obj);
        }
    }
    #[cold]
    #[inline(never)]
    fn enqueue_known_active_slow(&self, queue: &mut SATBMarkQueue, obj: *mut HeapObjectHeader) {
        self.handle_zero_index(queue);
        self.retry_enqueue(queue, obj.cast());
    }

    pub fn set_process_completed_buffers_threshold(&self, value: usize) {
        let mut scaled_value = value.wrapping_shl(1);
        if scaled_value.wrapping_shr(1) != value {
            scaled_value = usize::MAX;
        }

        self.process_completed_buffers_threshold
            .store(scaled_value | 1, Ordering::Relaxed);
    }

    pub fn set_buffer_enqueue_threshold_percentage(&self, value: usize) {
        let size = self.buffer_size();
        let enqueue_qty = (size * value) / 100;
        self.buffer_enqueue_threshold
            .store((size - enqueue_qty).max(1), Ordering::Relaxed);
    }

    pub fn abandon_completed_buffers(&self) {
        self.lock.lock();
        let mut buffers_to_delete = self.list.pop_all();

        unsafe {
            while !buffers_to_delete.is_null() {
                let bn = buffers_to_delete;
                buffers_to_delete = (*bn).next();
                (*bn).set_next(null_mut());
                self.deallocate_buffer(bn);
            }
            self.lock.unlock();
        }
    }

    pub fn abandon_partial_marking(&self) {
        assert!(
            SafepointSynchronize::is_at_safepoint(),
            "Must be at safepoint"
        );
        self.abandon_completed_buffers();

        threads()
            .get()
            .threads
            .iter()
            .copied()
            .for_each(|thread| unsafe {
                self.reset_queue((*thread).satb_mark_queue_mut());
            });
    }
}

impl PtrQueueSetImpl for SATBMarkQueueSet {
    type Queue = SATBMarkQueue;
    fn new(allocator: crate::utils::ptr_queue::Allocator) -> Self {
        Self {
            allocator,
            list: LockFreeStack::new(),
            count_and_process_flag: AtomicUsize::new(0),
            process_completed_buffers_threshold: AtomicUsize::new(0),
            buffer_enqueue_threshold: AtomicUsize::new(0),
            all_active: AtomicBool::new(false),
            lock: Lock::INIT,
        }
    }

    fn allocator(&self) -> &Allocator {
        &self.allocator
    }
    fn enqueue_completed_buffer(&self, node: *mut BufferNode) {
        self.lock.lock();
        increment_count(
            &self.count_and_process_flag,
            self.process_completed_buffers_threshold
                .load(Ordering::Relaxed),
        );

        unsafe {
            self.list.push(node);
            self.lock.unlock();
        }
    }
}

fn increment_count(cf: &AtomicUsize, threshold: usize) {
    let mut old;
    let mut value = cf.load(Ordering::Relaxed);
    loop {
        old = value;
        value += 2;

        assert!(value > old, "overflow");
        if value > threshold {
            value |= 1;
        }
        
        match cf.compare_exchange_weak(old, value, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => {
                println!("store: {}->{} ({} {} {})", old, value, cf.load(Ordering::Relaxed) >> 1, threshold, value | 1);
                break;
            },
            Err(new_val) => value = new_val,
        }
    }
}

fn decrement_count(cf: &AtomicUsize) {
    let mut old;
    let mut value = cf.load(Ordering::Relaxed);
    println!("decrement");
    loop {
        assert!((value >> 1) != 0, "underflow: {}", value);
        old = value;
        value -= 2;
        if value <= 1 {
            value = 0;
        }

        match cf
            .compare_exchange(old, value, Ordering::SeqCst, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(new_value) => value = new_value,
        }
        
    }
}
