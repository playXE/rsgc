use std::{
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicUsize},
};

use parking_lot::lock_api::RawMutex;

use crate::{
    heap::{sweeper::SweepGarbageClosure, ConcurrentPhase, PausePhase},
    object::HeapObjectHeader,
    sync::suspendible_thread_set::SuspendibleThreadSetJoiner,
};

use super::{
    card_table::{age_card_visitor, CardTable},
    heap::{heap, Heap, HeapRegionClosure},
    mark::{MarkTask, MarkingTask, Terminator},
    root_processor::RootsCollector,
    safepoint::SafepointSynchronize,
    sweeper::Sweep,
    thread::{threads, Thread},
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

            let threads = SafepointSynchronize::begin();
            log::debug!(target: "gc-safepoint", "stopped the world ({} thread(s)) in {} ms", threads.len(), start.elapsed().as_millis());
            let mark = ConcMark::new();

            {
                let phase = PausePhase::new("Init Mark");
                // Sets all mark bits to zero
                self.heap.prepare_gc();

                for thread in threads.iter().copied() {
                    (*thread).tlab.retire((*thread).id);
                    (*thread).toggle_write_barrier(true);
                }
                // mark roots in STW
                mark.collect_roots(&threads);
                self.heap.set_concurrent_mark_in_progress(true);
                SafepointSynchronize::end(threads);
                drop(phase);
            }
            let phase = ConcurrentPhase::new("marking");
            // mark objects concurrently
            mark.mark();
            if self.check_cancellation_and_abort(DegenPoint::ConcurrentMark) {
                // degenrate cycle will do the rest of work in STW cycle
                return false;
            }

            drop(phase);

            // finish concurrent marking in safepoint
            let threads = SafepointSynchronize::begin();
            {
                let phase = PausePhase::new("Final Mark");
                for thread in threads.iter().copied() {
                    (*thread).tlab.retire((*thread).id);
                }
                self.heap.set_concurrent_mark_in_progress(false);
                mark.collect_roots(&threads); // remark roots
                mark.finish(&threads); // drain remaining objects and SATB queues
                drop(phase);
                log::debug!(target: "gc", "Concurrent mark end in {} msecs", start.elapsed().as_millis());
            }

            // Phase 2: Sweep
            {
                let phase = PausePhase::new("Prepare unswept regions");
                let sweep_start = std::time::Instant::now();
                let prep = PrepareUnsweptRegions {};

                self.heap.heap_region_iterate(&prep);
                drop(phase);
                SafepointSynchronize::end(threads);
                let phase = ConcurrentPhase::new("Sweeping");
                let sweep = SweepGarbageClosure {
                    heap: heap(),
                    live: AtomicUsize::new(0),
                    concurrent: true,
                    freed: AtomicUsize::new(0),
                };
                self.heap
                    .parallel_heap_region_iterate_with_cancellation(&sweep);
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
                drop(phase);
                
                log::debug!(target: "gc", "Concurrent GC end in {} msecs ({} msecs sweep), Region: {} live, {} freed", start.elapsed().as_millis(), sweep_end.as_millis(), sweep.live.load(atomic::Ordering::Relaxed), sweep.freed.load(atomic::Ordering::Relaxed));
            }

            true
        }
    }
}

pub struct PrepareUnsweptRegions {}

impl HeapRegionClosure for PrepareUnsweptRegions {
    fn heap_region_do(&self, r: *mut super::region::HeapRegion) {
        unsafe {
            if (*r).is_regular() || (*r).is_humongous_start() {
                (*r).to_sweep = true;
                // no lock required, executed in safepoint
                heap()
                    .free_set_mut()
                    .mutator_free_bitmap
                    .set((*r).index(), false);
            }
        }
    }
}

pub struct ConcMark {}

impl ConcMark {
    pub fn new() -> Self {
        Self {}
    }

    pub fn cancel(&self, threads: &[*mut Thread]) {
        unsafe {
            let ix = heap().options().max_satb_buffer_size;
            for thread in threads.iter().copied() {
                (*thread).satb_mark_queue_mut().set_index(ix);
                (*thread).toggle_write_barrier(false);
            }
        }
    }

    pub fn collect_roots(&self, threads: &[*mut Thread]) {
        let heap = heap();
        let mut mark_stack = {
            let mut root_collector = RootsCollector::new(heap);
            root_collector.collect(threads);
            root_collector.get_mark_stack()
        };

        let injector = heap.marking_context().mark_queues().injector();
        while let Some(obj) = mark_stack.pop() {
            assert!(heap.is_in(obj.cast()), "non-heap pointer: {:p}", obj);
            injector.push(MarkTask::new(obj, false, false));
        }
    }

    pub fn scan_gray_objects(&self, paused: bool, minimum_age: u8) -> bool {
        let clear = paused;

        let _heap = heap();
        let has_work = AtomicBool::new(false);
        _heap.parallel_heap_region_iterate(&ScanCardsForRegionsTask {
            heap: heap(),
            minimum_age,
            has_work: &has_work,
        });

        if clear {
            _heap
                .card_table()
                .clear_card_range(_heap.mem_start() as _, _heap.mem_end() as _);
        }
        has_work.load(atomic::Ordering::Relaxed)
    }

    pub fn recursive_mark_dirty_objects(&mut self) {}

    pub fn finish(&self, threads: &[*mut Thread]) {
        let heap = heap();

        let mc = heap.marking_context();

        let terminator = Terminator::new(mc.mark_queues().nworkers());
        for thread in threads.iter().copied() {
            unsafe {
                (*thread).flush_ssb();
                (*thread).toggle_write_barrier(false);
            }
        }

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
        //heap.card_table()
        //    .clear_card_range(heap.mem_start() as _, heap.mem_end() as _);
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
            /*
            // now when marking is done we pre-clean dirty cards

            let heap = super::heap::heap();

            // Ages cards: DIRTY become AGED and CLEAN cards are ignored
            heap.card_table().modify_cards_atomic(
                heap.mem_start() as _,
                heap.mem_end() as _,
                age_card_visitor,
                |_, _, _| {},
            );
            let has_work = AtomicBool::new(false);
            heap.parallel_heap_region_iterate(&ScanCardsForRegionsTask {
                heap: super::heap::heap(),
                minimum_age: CardTable::CARD_DIRTY - 1,
                has_work: &has_work,
            });

            if has_work.load(atomic::Ordering::Relaxed) {
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
            }*/
        });
    }
}

pub struct ScanCardsForRegionsTask<'a> {
    heap: &'static Heap,
    minimum_age: u8,
    has_work: &'a AtomicBool,
}

unsafe impl<'a> Sync for ScanCardsForRegionsTask<'a> {}
unsafe impl<'a> Send for ScanCardsForRegionsTask<'a> {}

impl<'a> HeapRegionClosure for ScanCardsForRegionsTask<'a> {
    fn heap_region_do(&self, r: *mut super::region::HeapRegion) {
        unsafe {
            let injector = self.heap.marking_context().mark_queues().injector();
            let scan_start = (*r).bottom();
            let scan_end = (*r).end();
            if (*r).is_humongous_start() {
                let object = (*r).bottom() as *mut HeapObjectHeader;
                let card = self.heap.card_table().card_from_addr(object as _);
                if card.read() >= self.minimum_age {
                    if self.heap.marking_context().mark(object) {
                        self.has_work.store(true, atomic::Ordering::Relaxed);
                        injector.push(MarkTask::new(object, false, false));
                    }
                }
            } else if (*r).is_humongous_cont() || (*r).is_empty() || (*r).is_trash() {
                // no-op
            } else {
                self.heap.card_table().scan::<false>(
                    &(*r).object_start_bitmap,
                    scan_start as _,
                    scan_end as _,
                    |object| {
                        if self.heap.marking_context().mark(object.cast()) {
                            self.has_work.store(true, atomic::Ordering::Relaxed);
                            injector.push(MarkTask::new(object.cast(), false, false));
                        }
                    },
                    self.minimum_age,
                );
            }
        }
    }
}
