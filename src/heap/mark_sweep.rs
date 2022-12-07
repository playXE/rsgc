use std::mem::size_of;
use std::sync::atomic::AtomicUsize;

use super::heap::{heap, Heap};
use super::safepoint;
use crate::heap::controller::GCMode;
use crate::heap::sweeper::SweepGarbageClosure;
use crate::heap::thread::wait_for_the_world;
use crate::heap::thread::ThreadInfo;
use crate::object::HeapObjectHeader;
use crate::traits::Visitor;
use parking_lot::lock_api::RawMutex;
use parking_lot::MutexGuard;
pub struct MarkSweep {
    heap: &'static mut Heap,
    mark_stack: Vec<*mut HeapObjectHeader>,
}

impl MarkSweep {
    pub fn new() -> Self {
        Self {
            heap: heap(),
            mark_stack: Vec::with_capacity(128)
        }
    }

    pub unsafe fn do_collect(&mut self, mode: GCMode) {
        let start = std::time::Instant::now();
        while !safepoint::enter() {
            // Users can use our safepoint infrastructure as well and safepoint::enter() can return false
            // in cases like cleaning JIT caches on the user side.
            std::thread::yield_now();
        }

        let mut threads = wait_for_the_world();
        log::debug!(target: "gc-safepoint", "stopped the world in {} ms", start.elapsed().as_millis());
        // Phase 0: retire TLABs
        for thread in threads.iter().copied() {
            (*thread).tlab.retire();
        }
        {
            // Phase 1: Mark
            self.do_mark(&mut threads);
        }

        {
            // Phase 2: Process weak references
            self.heap.process_weak_refs();
        }

        // Phase 3: Sweep
        let closure = SweepGarbageClosure {
            concurrent: mode == GCMode::ConcurrentSweep,
            heap: heap(),
            live: AtomicUsize::new(0),
        };
        match mode {
            GCMode::FullSTW => {
                self.heap.heap_region_iterate(&closure);
                self.heap.lock.lock();
                self.heap.free_set_mut().recycle_trash();
                self.heap.free_set_mut().rebuild();
                self.heap.lock.unlock();
                
            }

            GCMode::ConcurrentSweep => {
                
                self.heap.heap_region_iterate(&closure);
                self.heap.lock.lock();
                self.heap.free_set_mut().recycle_trash();
                self.heap.free_set_mut().rebuild();
                self.heap.lock.unlock();
            }
            _ => unreachable!(),
        }

        log::debug!(target: "gc", "Mark-and-Sweep done in {} ms, {} live regions after sweep", start.elapsed().as_millis(), closure.live.load(std::sync::atomic::Ordering::Relaxed));
        safepoint::end();
    }

    unsafe fn do_mark(&mut self, threads: &mut MutexGuard<'_, Vec<*mut ThreadInfo>>) {
        for thread in threads.iter().copied() {
            self.mark_thread(thread);
        }

        heap().walk_global_roots(self);

        while let Some(object) = self.mark_stack.pop() {
            (*object).visit(self);
        }
    }

    unsafe fn mark_thread(&mut self, thread: *mut ThreadInfo) {
        let mut begin = (*thread).stack_start();
        let mut end = (*thread).last_sp();
        if end.is_null() {
            return;
        }
        if begin > end {
            std::mem::swap(&mut begin, &mut end);
        }
        
        while begin < end {
            self.try_mark_conservatively(begin.cast::<*mut u8>().read());
            begin = begin.add(size_of::<usize>());
        }
    }

    fn try_mark(&mut self, object: *const u8) {
        unsafe {
            let header = object.sub(size_of::<HeapObjectHeader>()) as *mut HeapObjectHeader;

            if (*header).is_marked() {
                return;
            }

            (*header).set_marked();

            self.mark_stack.push(header);
        }
    }

    fn try_mark_conservatively(&mut self, pointer: *mut u8) {
        unsafe {
            if !self.heap.is_in(pointer) {
                return;
            }
            
            let object = self.heap.object_start(pointer);
            if object.is_null() {
                return;
            }

            self.try_mark(object.add(1) as *const u8);
        }
    }
}

impl Visitor for MarkSweep {
    fn visit(&mut self, object: *const u8) {
        self.try_mark(object);
    }

    fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize) {
        for i in 0..len {
            unsafe {
                let ptr = *ptrs.add(i);
                self.try_mark_conservatively(ptr as *mut u8);
            }
        }
    }
}
