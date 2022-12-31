/* 
use std::sync::atomic::{AtomicBool, AtomicUsize};

use crate::prelude::HeapOptions;

use super::heap::Heap;

const GC_WORKERS_PER_MUTATOR: usize = 2;
const PARALLEL_WORKER_THREADS: AtomicUsize = AtomicUsize::new(0);
const PARALLEL_WORKER_THREADS_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub struct WorkerPolicy;

impl WorkerPolicy {
    #[allow(unused_mut)]
    fn nof_parallel_worker_threads(
        num: usize,
        den: usize,
        switch_pt: usize,
        args: &HeapOptions,
    ) -> usize {
        if args.parallel_gc_threads == 0 {
            let mut threads;

            let ncpus = std::thread::available_parallelism()
                .map(|x| x.get())
                .unwrap_or(1);

            // For very large machines, there are diminishing returns
            // for large numbers of worker threads.  Instead of
            // hogging the whole system, use a fraction of the workers for every
            // processor after the first 8.  For example, on a 72 cpu machine
            // and a chosen fraction of 5/8
            // use 8 + (72 - 8) * (5/8) == 48 worker threads.
            threads = if ncpus <= switch_pt {
                ncpus
            } else {
                switch_pt + ((ncpus - switch_pt) * num) / den
            };

            #[cfg(target_pointer_width = "32")]
            {
                // On 32-bit binaries the virtual address space available to the JVM
                // is usually limited to 2-3 GB (depends on the platform).
                // Do not use up address space with too many threads (stacks and per-thread
                // data). Note that x86 apps running on Win64 have 2 stacks per thread.
                // GC may more generally scale down threads by max heap size (etc), but the
                // consequences of over-provisioning threads are higher on 32-bit JVMS,
                // so add hard limit here:
                threads = threads.min(2 * switch_pt);
            }

            threads
        } else {
            args.parallel_gc_threads
        }
    }

    /// Calculates and returns the number of parallel GC threads. May
    /// be CPU-architecture-specific.
    fn calc_parallel_worker_threads(args: &HeapOptions) -> usize {
        let den = 8; // apparently default denominator value in hotspot atm
        Self::nof_parallel_worker_threads(5, den, 8, args)
    }

    pub fn parallel_worker_threads(args: &HeapOptions) -> usize {
        if !PARALLEL_WORKER_THREADS_INITIALIZED.load(atomic::Ordering::Relaxed) {
            if args.parallel_gc_threads == 0 {
                let threads = Self::calc_parallel_worker_threads(args);
                PARALLEL_WORKER_THREADS.store(threads, atomic::Ordering::Relaxed);
            } else {
                PARALLEL_WORKER_THREADS.store(args.parallel_gc_threads, atomic::Ordering::Relaxed);
            }

            PARALLEL_WORKER_THREADS_INITIALIZED.store(true, atomic::Ordering::Relaxed);
        }

        PARALLEL_WORKER_THREADS.load(atomic::Ordering::Relaxed)
    }

    ///  If the number of GC threads was set on the command line, use it.
    ///  Else
    ///    Calculate the number of GC threads based on the number of Java threads.
    ///    Calculate the number of GC threads based on the size of the heap.
    ///    Use the larger.
    pub fn calc_default_active_workers(
        total_workers: usize,
        min_workers: usize,
        active_workers: usize,
        application_workers: usize,
    ) -> usize {
        // If the user has specifically set the number of GC threads, use them.

        // If the user has turned off using a dynamic number of GC threads
        // or the users has requested a specific number, set the active
        // number of workers to all the workers.

        let new_active_workers = total_workers;
        let prev_active_workers = active_workers;
        let mut active_workers_by_mt = 0;
        let mut active_workers_by_heap_size = 0;

        active_workers_by_mt = (GC_WORKERS_PER_MUTATOR * application_workers).max(min_workers);

        let heap = super::heap::heap();

        active_workers_by_heap_size = 2.max(heap.max_capacity() / )
    }
}
*/