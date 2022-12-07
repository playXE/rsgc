use std::{sync::{atomic::{AtomicBool, Ordering, AtomicUsize}, Arc}, time::{Instant, Duration}};

use parking_lot::{Mutex, Condvar, lock_api::RawMutex};

use crate::{sync::monitor::Monitor, formatted_size, heap::mark_sweep::MarkSweep};

use super::{shared_vars::SharedFlag, concurrent_thread::ConcurrentGCThread, heap::heap, AllocRequest};



#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum GCMode {
    None,
    /// Happens when user explicitly requested GC or alloc failure
    FullSTW,
    /// Happens when threshold is reached
    ConcurrentSweep, 
}

pub struct ControlThread {
    should_terminate: AtomicBool,
    has_terminated: AtomicBool,

    gc_requested: SharedFlag,
    alloc_failure_gc: SharedFlag,
    graceful_shutdown: SharedFlag,
    heap_changed: SharedFlag,
    gc_id: AtomicUsize,

    terminator_cond: Condvar,
    terminator_lock: Mutex<()>,

    alloc_failure_waiters_lock: Monitor<()>,
    gc_waiters_lock: Monitor<()>,
}

impl ControlThread {
    pub fn new() -> &'static mut Self {
        let thread = Box::leak(Box::new(Self {
            should_terminate: AtomicBool::new(false),
            has_terminated: AtomicBool::new(false),
            gc_requested: SharedFlag::new(),
            alloc_failure_gc: SharedFlag::new(),
            graceful_shutdown: SharedFlag::new(),
            heap_changed: SharedFlag::new(),

            terminator_cond: Condvar::new(),
            terminator_lock: Mutex::new(()),
            gc_id: AtomicUsize::new(0),
            alloc_failure_waiters_lock: Monitor::new(()),
            gc_waiters_lock: Monitor::new(())
        }));

        let ptr = thread as *mut ControlThread as usize;
        let sync_with_child = Arc::new((Mutex::new(false), Condvar::new()));
        let sync_with_child_2 = sync_with_child.clone();
        std::thread::spawn(move || {
            unsafe {
                {
                    let sync_with_child = sync_with_child_2;

                    {
                        let mut lock = sync_with_child.0.lock();
                        *lock = true;
                        drop(lock);
                        
                        sync_with_child.1.notify_one();
                    }

                    std::mem::forget(sync_with_child);
                }
                let ptr = ptr as *mut ControlThread;
                let controller = &mut *ptr;

                controller.run();
            }
        });
        // Wait for controller thread to be actually spawned
        let mut lock = sync_with_child.0.lock();
        if !*lock {
            sync_with_child.1.wait(&mut lock);
        }

        thread 
    }

    pub fn prepare_for_graceful_shutdown(&self) {
        self.graceful_shutdown.set();
    }

    pub fn in_graceful_shutdown(&self) -> bool {
        self.graceful_shutdown.is_set()
    }

    pub fn notify_heap_changed(&self) {
        if self.heap_changed.is_unset() {
            self.heap_changed.set();
        }
    }

    pub fn reset_gc_id(&self) {
        self.gc_id.store(0, Ordering::Relaxed);
    }

    pub fn update_gc_id(&self) {
        self.gc_id.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_gc_id(&self) -> usize {
        self.gc_id.load(Ordering::Relaxed)
    }


    pub fn handle_alloc_failure_gc(&self, req: &mut AllocRequest) {
        if self.alloc_failure_gc.try_set() {
            log::info!(
                target: "gc",
                "Failed to allocate {}",
                formatted_size(req.size())
            );
        }

        let mut ml = self.alloc_failure_waiters_lock.lock(true);
        while self.alloc_failure_gc.is_set() {
            ml.wait();
        }
    }

    /// Notify all mutators where allocation failed that GC is finished. 
    /// 
    /// Invoked by GC controller only
    /// 
    pub fn notify_alloc_failure_waiters(&self) {
        self.alloc_failure_gc.unset();
        self.alloc_failure_waiters_lock.lock(false).notify_all();
    }

    pub fn notify_gc_waiters(&self) {
        self.gc_requested.unset();
        self.gc_waiters_lock.lock(false).notify_all();
    }

    #[inline(never)]
    pub fn handle_requested_gc(&self) {
        let mut ml = self.gc_waiters_lock.lock(true);

        let mut current_gc_id = self.get_gc_id();
        let required_gc_id = current_gc_id + 1;
        
        while current_gc_id < required_gc_id {
            self.gc_requested.set();

            ml.wait();

            current_gc_id = self.get_gc_id();
        }
    }
}


impl ConcurrentGCThread for ControlThread {
    fn run(&mut self) {
        self.run_service();
        let lock = self.terminator_lock.lock();
        self.has_terminated.store(true, Ordering::Release);
        drop(lock);
        self.terminator_cond.notify_all();
    }

    fn run_service(&mut self) {

        let heap = heap();
        let mut last_sleep_adjust_time = Instant::now();
        let _last_shrink_time = Instant::now();
        let mut sleep = heap.options().control_interval_min;
        while !self.in_graceful_shutdown() && !self.should_terminate() {
            let alloc_failure_pending = self.alloc_failure_gc.is_set();
            let explicit_gc_requested = self.gc_requested.is_set();

            let mut mode = GCMode::None;

            if alloc_failure_pending {
                log::info!(target: "gc", "Trigger: Handle allocation failure");
                mode = GCMode::FullSTW;
            } else if explicit_gc_requested {
                log::info!(target: "gc", "Trigger: Explicit GC request");
                mode = GCMode::FullSTW;
            } else {
                if heap.should_start_gc() {
                    mode = GCMode::FullSTW;
                }
            }

            let gc_requested = mode != GCMode::None;

            if gc_requested {
                unsafe {
                    self.update_gc_id();
                    heap.set_allocated(0);

                    {
                        heap.lock.lock();
                        heap.free_set().log_status();
                        heap.lock.unlock();
                    }

                   
                    {
                        let mut ms = MarkSweep::new();
                        ms.do_collect(mode);
                        
                    }

                    // If this was the requested GC cycle, notify waiters about it
                    if explicit_gc_requested {
                        
                        self.notify_gc_waiters();
                    }

                    // If this was the allocation failure GC cycle, notify waiters about it
                    if alloc_failure_pending {
                        self.notify_alloc_failure_waiters();
                    }

                    {
                        heap.lock.lock();
                        heap.free_set().log_status();
                        heap.lock.unlock();
                    }
                }
            }

            let current = std::time::Instant::now();

            {
                // todo: uncommit empty regions if threshold/time reached or explicit GC
            }

            // Wait before performing the next action. If allocation happened during this wait,
            // we exit sooner, to let heuristics re-evaluate new conditions. If we are at idle,
            // back off exponentially.
            if self.heap_changed.try_unset() {
                sleep = heap.options().control_interval_min
            } else if ((current - last_sleep_adjust_time)).as_millis() as usize > heap.options().control_interval_adjust_period {
                last_sleep_adjust_time = current;
                sleep = heap.options().control_interval_max.min(1.max(sleep * 2));
            };
            
            std::thread::sleep(Duration::from_millis(sleep as _));
        }

        while !self.should_terminate() {
            std::thread::yield_now();
        }
        log::debug!(target: "gc", "Controller thread terminated");
    }

    fn should_terminate(&self) -> bool {
        self.should_terminate.load(Ordering::Acquire)
    }

    fn has_terminated(&self) -> bool {
        self.has_terminated.load(Ordering::Acquire)
    }

    fn stop(&mut self) {
        self.should_terminate.store(true, Ordering::Release);
        self.stop_service();
        let mut lock = self.terminator_lock.lock();
        while !self.has_terminated.load(Ordering::Relaxed) {
            self.terminator_cond.wait(&mut lock);
        }
    }

    fn stop_service(&mut self) {
        // no-op
    }

    fn create_and_start(&mut self) {
        
    }
}