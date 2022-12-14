use std::mem::size_of;
use std::sync::atomic::AtomicUsize;

use super::heap::{heap, Heap};
use super::mark::STWMark;
use super::marking_context::MarkingContext;
use super::safepoint;
use crate::heap::concurrent_gc::PrepareUnsweptRegions;
use crate::heap::controller::GCMode;
use crate::heap::safepoint::SafepointSynchronize;
use crate::heap::sweeper::SweepGarbageClosure;
use crate::heap::thread::ThreadInfo;
use crate::object::HeapObjectHeader;
use crate::traits::Visitor;
use parking_lot::lock_api::RawMutex;
use parking_lot::MutexGuard;

/// This implements Full GC (e.g. when invoking [Heap::request_gc](crate::heap::heap::Heap::request_gc)) using a mark-and-sweep algorithm.
/// 
/// It is done in fully stop-the-world.
pub struct FullGC {
    heap: &'static mut Heap,
    mark_stack: Vec<*mut HeapObjectHeader>,
    mark_ctx: &'static MarkingContext,
}

impl FullGC {
    pub fn new() -> Self {
        Self {
            heap: heap(),
            mark_ctx: heap().marking_context(),
            mark_stack: Vec::with_capacity(128),
        }
    }

    pub unsafe fn do_collect(&mut self) {
        let start = std::time::Instant::now();

        let threads = SafepointSynchronize::begin();
        log::debug!(target: "gc-safepoint", "stopped the world ({} thread(s)) in {} ms", threads.len(), start.elapsed().as_millis());
        self.heap.prepare_gc();
        // Phase 0: retire TLABs
        for thread in threads.iter().copied() {
            (*thread).tlab.retire((*thread).id);
        }
        {
            // Phase 1: Mark
            self.do_mark(threads);
        }

        {
            // Phase 2: Process weak references
            self.heap.process_weak_refs();
        }

        let prep = PrepareUnsweptRegions {};

        self.heap.heap_region_iterate(&prep);

        // Phase 3: Sweep
        let closure = SweepGarbageClosure {
            concurrent: false,
            heap: heap(),
            live: AtomicUsize::new(0),
            freed: AtomicUsize::new(0),
        };

        if self.heap.options().parallel_sweep {
            self.heap.parallel_heap_region_iterate(&closure);
        } else {
            self.heap.heap_region_iterate(&closure);
        }

        self.heap.free_set_mut().recycle_trash();

        self.heap.lock.lock();
        self.heap.free_set_mut().rebuild();
        self.heap.lock.unlock();

        log::debug!(target: "gc", "STW Mark-and-Sweep done in {} ms, {} live regions after sweep", start.elapsed().as_millis(), closure.live.load(std::sync::atomic::Ordering::Relaxed));
        SafepointSynchronize::end();
    }

    unsafe fn do_mark(&mut self, threads: &[*mut ThreadInfo]) {
        let mark = STWMark::new();
        mark.mark(&threads);
    }
    /*
    unsafe fn start_parallel_marking(&mut self) {
        let nworkers = self.heap.workers().workers();
        let mut workers = Vec::with_capacity(nworkers);
        let mut stealers = Vec::with_capacity(nworkers);

        let injector = Injector::new();

        for _ in 0..nworkers {
            let w = Worker::new_lifo();
            let s = w.stealer();
            workers.push(w);
            stealers.push(s);
        }

        for obj in self.mark_stack.drain(..) {
            injector.push(obj as usize);
        }

        let terminator = Terminator::new(nworkers);

        self.heap.workers().scoped(|scope| {
            for (task_id, worker) in workers.into_iter().enumerate() {
                let injector = &injector;
                let stealers = &stealers;
                let terminator = &terminator;

                scope.execute(move || {
                    let mut task = MarkingTask {
                        task_id,
                        local: Segment::new(),
                        worker,
                        injector,
                        stealers,
                        terminator,
                        marked: 0,
                        heap: heap(),
                        mark_ctx: heap().marking_context(),
                    };

                    task.run();
                });
            }
        });


    }*/

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

            if self.mark_ctx.mark(header) {
                self.mark_stack.push(header);
            }
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

impl Visitor for FullGC {
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
