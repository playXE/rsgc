use std::sync::atomic::AtomicUsize;

use parking_lot::lock_api::RawMutex;

use crate::{
    heap::sweeper::SweepGarbageClosure, sync::suspendible_thread_set::SuspendibleThreadSetJoiner,
};

use super::{
    heap::{heap, Heap, HeapRegionClosure},
    mark::{MarkingTask, Terminator},
    root_processor::RootsCollector,
    safepoint::SafepointSynchronize,
    sweeper::Sweep,
    thread::ThreadInfo,
    DegenPoint,
};

pub struct ConcurrentGC {
    degen_point: DegenPoint,
    heap: &'static mut Heap,
}

impl ConcurrentGC {
    pub fn new() -> Self {
        Self {
            degen_point: DegenPoint::Unset,
            heap: heap(),
        }
    }

    pub fn degen_point(&self) -> DegenPoint {
        self.degen_point
    }

    fn check_cancellation_and_abort(&mut self, point: DegenPoint) -> bool {
        if self.heap.cancelled_gc() {
            self.degen_point = point;
            return true;
        }

        false
    }

    pub fn collect(&mut self) -> bool {
        unsafe {
            let start = std::time::Instant::now();
            // Phase 1: Mark
            {
                let threads = SafepointSynchronize::begin();
                log::debug!(target: "gc-safepoint", "stopped the world ({} thread(s)) in {} ms", threads.len(), start.elapsed().as_millis());
                let mark = ConcMark::new();

                {
                    // Sets all mark bits to zero
                    self.heap.prepare_gc();

                    for thread in threads.iter().copied() {
                        (*thread).tlab.retire((*thread).id);
                    }
                    // mark roots in STW
                    mark.collect_roots(threads);
                    self.heap.set_concurrent_mark_in_progress(true);
                    SafepointSynchronize::end();
                }
                // mark objects concurrently
                mark.mark();
                if self.check_cancellation_and_abort(DegenPoint::ConcurrentMark) {
                    // degenrate cycle will do the rest of work in STW cycle
                    return false;
                }

                // finish concurrent marking in safepoint
                {
                    let threads = SafepointSynchronize::begin();
                    for thread in threads.iter().copied() {
                        (*thread).tlab.retire((*thread).id);
                    }
                    self.heap.set_concurrent_mark_in_progress(false);
                    mark.collect_roots(threads); // remark roots
                    mark.finish(threads); // drain remaining objects and SATB queues
                    log::debug!(target: "gc", "Concurrent mark end in {} msecs", start.elapsed().as_millis());
                }
            }
            // Phase 2: Sweep
            {

                
                let sweep_start = std::time::Instant::now();
                let prep = PrepareUnsweptRegions {};

                self.heap.heap_region_iterate(&prep);

                SafepointSynchronize::end();
                let sweep = SweepGarbageClosure {
                    heap: heap(),
                    live: AtomicUsize::new(0),
                    concurrent: true,
                    freed: AtomicUsize::new(0),
                };
                self.heap.parallel_heap_region_iterate_with_cancellation(&sweep);
                let sweep_end = sweep_start.elapsed();
                
                
                if self.check_cancellation_and_abort(DegenPoint::ConcurrentSweep) {
                    return false;
                }

                // Finish concurrent sweeping by rebuilding free-region set.
                // Do not recycle trashed regions here as it is considered too costly and instead
                // we will do it at mutator kinda like lazy-sweeping.
                self.heap.lock.lock();
                self.heap.free_set_mut().rebuild();
                self.heap.lock.unlock();

                log::debug!(target: "gc", "Concurrent GC end in {} msecs ({} msecs sweep), Region: {} live, {} freed", start.elapsed().as_millis(), sweep_end.as_millis(), sweep.live.load(atomic::Ordering::Relaxed), sweep.freed.load(atomic::Ordering::Relaxed));
            }

            true
        }
    }
}

pub struct PrepareUnsweptRegions {
    
}

impl HeapRegionClosure for PrepareUnsweptRegions {
    fn heap_region_do(&self, r: *mut super::region::HeapRegion) {
        unsafe {
            if (*r).is_regular() || (*r).is_humongous_start() {
                (*r).to_sweep = true;
                // no lock required, executed in safepoint
                heap().free_set_mut().mutator_free_bitmap.set((*r).index(), false);
            }
        }
    }
}

pub struct ConcMark {}

impl ConcMark {
    pub fn new() -> Self {
        Self {}
    }

    pub fn collect_roots(&self, threads: &[*mut ThreadInfo]) {
        let heap = heap();
        let mut mark_stack = {
            let mut root_collector = RootsCollector::new(heap);
            root_collector.collect(threads);
            root_collector.get_mark_stack()
        };

        let injector = heap.marking_context().mark_queues().injector();
        while let Some(obj) = mark_stack.pop() {
            injector.push(obj as usize);
        }
    }

    pub fn finish(&self, threads: &[*mut ThreadInfo]) {
        for thread in threads.iter().copied() {
            unsafe {
                (*thread).satb_flush();
            }
        }
        let heap = heap();

        let mc = heap.marking_context();

        let terminator = Terminator::new(mc.mark_queues().nworkers());

        // blocking call, waits for marking task to complete.
        heap.workers().scoped(|scope| {
            for task_id in 0..mc.mark_queues().nworkers() {
                let terminator = &terminator;

                scope.execute(move || {
                    let stsj = SuspendibleThreadSetJoiner::new(false);
                    let mut task = MarkingTask::new(
                        task_id,
                        &terminator,
                        super::heap::heap(),
                        super::heap::heap().marking_context(),
                    );

                    task.run::<false>();
                    drop(stsj);
                });
            }
        });
    }

    pub fn mark(&self) {
        let heap = heap();

        let mc = heap.marking_context();

        let terminator = Terminator::new(mc.mark_queues().nworkers());

        // blocking call, waits for marking task to complete.
        heap.workers().scoped(|scope| {
            for task_id in 0..mc.mark_queues().nworkers() {
                let terminator = &terminator;

                scope.execute(move || {
                    let stsj = SuspendibleThreadSetJoiner::new(true);
                    let mut task = MarkingTask::new(
                        task_id,
                        &terminator,
                        super::heap::heap(),
                        super::heap::heap().marking_context(),
                    );

                    task.run::<true>();
                    drop(stsj);
                });
            }
        });
    }
}
