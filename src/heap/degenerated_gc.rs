use std::sync::atomic::AtomicUsize;

use atomic::Ordering;
use parking_lot::lock_api::RawMutex;

use crate::heap::{
    concurrent_gc::{ConcMark, PrepareUnsweptRegions},
    mark::STWMark,
    sweeper::{Sweep, SweepGarbageClosure},
};

use super::{
    heap::{heap, Heap},
    safepoint::SafepointSynchronize,
    DegenPoint,
};

pub struct DegeneratedGC {
    degen_point: DegenPoint,
    heap: &'static mut Heap,
}

impl DegeneratedGC {
    pub fn new(degen_point: DegenPoint) -> Self {
        Self {
            degen_point,
            heap: heap(),
        }
    }

    pub unsafe fn collect(&mut self) {
        let start = std::time::Instant::now();

        let threads = SafepointSynchronize::begin();
        log::debug!(target: "gc-safepoint", "stopped the world ({} thread(s)) in {} ms", threads.len(), start.elapsed().as_millis());
        self.heap.clear_cancelled_gc();

        for thread in threads.iter().copied() {
            (*thread).tlab.retire((*thread).id);
        }

        if self.degen_point == DegenPoint::OutsideCycle {
            self.heap.prepare_gc();
            for thread in threads.iter().copied() {
                (*thread).satb_clear();
            }
            let mark = STWMark::new();

            mark.mark(threads);

            self.degen_point = DegenPoint::ConcurrentMark;
            self.heap.set_concurrent_mark_in_progress(false);
        }

        if self.degen_point == DegenPoint::ConcurrentMark {
            if self.heap.is_concurrent_mark_in_progress() {
                let mark = ConcMark::new();
                mark.collect_roots(threads); // remark roots and flush SATB buffers
                mark.finish(threads);

                self.heap.set_concurrent_mark_in_progress(false);
            }
            self.degen_point = DegenPoint::ConcurrentSweep;
        }

        if self.degen_point == DegenPoint::ConcurrentSweep {
            let prep = PrepareUnsweptRegions{};
            self.heap.heap_region_iterate(&prep);
            let sweep = SweepGarbageClosure {
                live: AtomicUsize::new(0),
                heap: heap(),
                freed: AtomicUsize::new(0),
                concurrent: false 
            };
            self.heap
                .parallel_heap_region_iterate(&sweep);

            self.heap.lock.lock();
            self.heap.free_set_mut().rebuild();
            self.heap.lock.unlock();
            log::debug!(target: "gc", "Degenerate GC end in {} msecs, {} regions alive after sweep", start.elapsed().as_millis(), sweep.live.load(Ordering::Relaxed));
        }

        SafepointSynchronize::end();
    }
}
