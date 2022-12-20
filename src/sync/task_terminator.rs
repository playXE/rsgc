use std::{
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize},
    time::Duration,
};

use atomic::Ordering;

use crate::{
    heap::thread::{Thread},
    utils::taskqueue::{TaskQueueSetSuper, TerminatorTerminator},
};

use super::monitor::Monitor;

struct DelayContext {
    yield_count: usize,
    hard_spin_count: usize,
    hard_spin_limit: usize,
}

impl DelayContext {
    fn reset_hard_spin_information(&mut self) {
        self.hard_spin_count = 0;
        self.hard_spin_limit = 4096 >> 10;
    }

    fn needs_sleep(&self) -> bool {
        self.yield_count >= 5000
    }

    fn new() -> Self {
        Self {
            yield_count: 0,
            hard_spin_count: 0,
            hard_spin_limit: 4096 >> 10,
        }
    }

    fn do_step(&mut self) {
        self.yield_count += 1;

        if self.hard_spin_count > 10 {
            std::thread::yield_now();
        } else {
            for _ in 0..self.hard_spin_limit {
                std::hint::spin_loop();
            }
            self.hard_spin_count += 1;
            self.hard_spin_limit = (2 * self.hard_spin_limit).min(4096);
        }
    }
}

pub struct TaskTerminator {
    n_threads: AtomicUsize,
    queue_set: *mut dyn TaskQueueSetSuper,
    offered_termination: AtomicUsize,
    blocker: Monitor<()>,
    spin_master: AtomicPtr<Thread>,
}

unsafe impl Send for TaskTerminator {}
unsafe impl Sync for TaskTerminator {}

impl TaskTerminator {
    pub fn new(n_threads: usize, queue_set: *mut dyn TaskQueueSetSuper) -> Self {
        Self {
            n_threads: AtomicUsize::new(n_threads),
            queue_set,
            blocker: Monitor::new(()),
            spin_master: AtomicPtr::new(null_mut()),
            offered_termination: AtomicUsize::new(0),
        }
    }

    pub fn reset_for_reuse(&self) {
        if self.offered_termination.load(Ordering::Relaxed) != 0 {
            self.offered_termination.store(0, Ordering::Relaxed);
        }
    }

    pub fn reset_for_reuse_(&self, threads: usize) {
        self.reset_for_reuse();
        self.n_threads.store(threads, Ordering::Relaxed);
    }

    pub fn exit_for_termination<T: TerminatorTerminator>(
        &self,
        tasks: usize,
        terminator: Option<&T>,
    ) -> bool {
        tasks > 0
            || terminator
                .map(|x| x.should_exit_termination())
                .unwrap_or(false)
    }

    pub fn tasks_in_queue_set(&self) -> usize {
        unsafe { (*self.queue_set).tasks() }
    }

    pub fn prepare_for_return(&self, thread: *mut Thread, mut tasks: usize) {
        if self.spin_master.load(Ordering::Relaxed) == thread {
            self.spin_master
                .store(std::ptr::null_mut(), Ordering::Relaxed);
        }

        if tasks >= self.offered_termination.load(Ordering::Relaxed) - 1 {
            self.blocker.notify_all();
        } else {
            while tasks > 1 {
                self.blocker.notify_one();
                tasks -= 1;
            }
        }
    }

    pub fn offer_termination<T: TerminatorTerminator>(&self, terminator: Option<&T>) -> bool {
        if self.n_threads.load(Ordering::Relaxed) == 1 {
            self.offered_termination.store(1, Ordering::Relaxed);
            return true;
        }

        let the_thread = Thread::current();

        let mut x = self.blocker.lock(false);
        self.offered_termination.fetch_add(1, Ordering::Relaxed);

        if self.offered_termination.load(Ordering::Relaxed)
            == self.n_threads.load(Ordering::Relaxed)
        {
            self.prepare_for_return(the_thread, usize::MAX);
            return true;
        }

        loop {
            if self.spin_master.load(Ordering::Relaxed).is_null() {
                self.spin_master.store(the_thread, Ordering::Relaxed);
                let mut delay_context = DelayContext::new();

                while !delay_context.needs_sleep() {
                    let tasks;
                    let should_exit_termination;

                    {
                        unsafe {
                            x.mutex.raw().unlock();
                            delay_context.do_step();
                            // Intentionally read the number of tasks outside the mutex since this
                            // is potentially a long operation making the locked section long.
                            tasks = self.tasks_in_queue_set();
                            should_exit_termination = self.exit_for_termination(tasks, terminator);
                            x.mutex.raw().lock(false);
                        }
                    }

                    if self.offered_termination.load(Ordering::Relaxed)
                        == self.n_threads.load(Ordering::Relaxed)
                    {
                        self.prepare_for_return(the_thread, usize::MAX);
                        return true;
                    } else if should_exit_termination {
                        self.prepare_for_return(the_thread, tasks);
                        self.offered_termination.fetch_sub(1, Ordering::Relaxed);
                        return false;
                    }
                }
                self.spin_master.store(null_mut(), Ordering::Relaxed);
            }

            let timed_out = x.wait_for(Duration::from_millis(1)).timed_out();

            if self.offered_termination.load(Ordering::Relaxed) == 1 {
                self.prepare_for_return(the_thread, usize::MAX);
                drop(x);
                return true;
            } else if !timed_out {
                self.prepare_for_return(the_thread, 0);
                self.offered_termination.fetch_sub(1, Ordering::Relaxed);
                drop(x);
                return false;
            } else {
                let tasks = self.tasks_in_queue_set();
                if self.exit_for_termination(tasks, terminator) {
                    self.prepare_for_return(the_thread, tasks);
                    self.offered_termination.fetch_sub(1, Ordering::Relaxed);
                    drop(x);
                    return false;
                }
            }
        }
    }
}
