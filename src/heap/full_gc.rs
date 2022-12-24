use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::atomic::AtomicUsize;

use super::heap::{heap, Heap};
use super::mark::STWMark;
use super::marking_context::MarkingContext;
use super::safepoint;
use crate::heap::concurrent_gc::PrepareUnsweptRegions;
use crate::heap::controller::GCMode;
use crate::heap::safepoint::SafepointSynchronize;
use crate::heap::sweeper::SweepGarbageClosure;
use crate::heap::thread::Thread;
use crate::heap::PausePhase;
use crate::system::object::HeapObjectHeader;
use crate::system::traits::{Object, Visitor};
use crate::system::weak_reference::WeakReference;
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
        let phase = PausePhase::new("Init Mark");
        self.heap.prepare_gc();
        self.heap.clear_cancelled_gc();
        // Phase 0: retire TLABs
        for thread in threads.iter().copied() {
            (*thread).tlab.retire((*thread).id);
        }
        drop(phase);
        {
            let phase = PausePhase::new("Marking");
            // Phase 1: Mark
            self.do_mark(&threads);
            drop(phase);
        }

        {
            let phase = PausePhase::new("Clean up weak references");
            WeakReference::<dyn Object>::process(|pointer| {
                let header = pointer.cast::<HeapObjectHeader>().sub(1);

                if self.heap.marking_context().is_marked(header) {
                    pointer
                } else {
                    null_mut()
                }
            });
            drop(phase);
        }


        let phase = PausePhase::new("Sweep");

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
        drop(phase);
        log::debug!(target: "gc", "STW Mark-and-Sweep done in {} ms, {} live regions after sweep", start.elapsed().as_millis(), closure.live.load(std::sync::atomic::Ordering::Relaxed));
        SafepointSynchronize::end(threads);

        
    }

    unsafe fn do_mark(&mut self, _threads: &[*mut Thread]) {
        let mark = STWMark::new();
        mark.mark();
    }
}
