use std::{
    sync::atomic::{AtomicBool, AtomicUsize},
    time::Instant,
};

use atomic::{Atomic, Ordering};

use super::{monitor::Monitor, semaphore::Sem};

/// A SuspendibleThreadSet is a set of threads that can be suspended.
/// A thread can join and later leave the set, and periodically yield.
/// If some thread (not in the set) requests, via synchronize(), that
/// the threads be suspended, then the requesting thread is blocked
/// until all the threads in the set have yielded or left the set. Threads
/// may not enter the set when an attempted suspension is in progress. The
/// suspending thread later calls desynchronize(), allowing the suspended
/// threads to continue.
pub struct SuspendibleThreadSet {}

static SUSPEND_ALL: AtomicBool = AtomicBool::new(false);
static NTHREADS: AtomicUsize = AtomicUsize::new(0);
static NTHREADS_STOPPED: AtomicUsize = AtomicUsize::new(0);
static SYNCHRONIZE_WAKEUP: Sem = unsafe { Sem::new_uninit() };
static STS_LOCK: Monitor<()> = Monitor::new(());

impl SuspendibleThreadSet {
    pub fn init() {
        let _ = SYNCHRONIZE_WAKEUP.init(0);
    }

    pub fn should_yield() -> bool {
        Self::suspend_all()
    }

    pub fn suspend_all() -> bool {
        SUSPEND_ALL.load(Ordering::Relaxed)
    }

    pub fn is_synchronized() -> bool {
        NTHREADS.load(Ordering::Relaxed) == NTHREADS_STOPPED.load(Ordering::Relaxed)
    }

    pub fn join() {
        let mut ml = STS_LOCK.lock(false);

        while Self::suspend_all() {
            ml.wait();
        }

        NTHREADS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn leave() {
        let ml = STS_LOCK.lock(false);
        assert!(NTHREADS.load(Ordering::Relaxed) > 0, "invalid");
        NTHREADS.fetch_sub(1, Ordering::Relaxed);
        if Self::suspend_all() && Self::is_synchronized() {
            SYNCHRONIZE_WAKEUP.signal(1);
        }
        drop(ml);
    }

    pub fn yield_() {
        let mut ml = STS_LOCK.lock(false);

        if Self::suspend_all() {
            NTHREADS_STOPPED.fetch_add(1, Ordering::Relaxed);
            if Self::is_synchronized() {
                SYNCHRONIZE_WAKEUP.signal(1);
            }
        }

        while Self::suspend_all() {
            ml.wait();
        }

        NTHREADS_STOPPED.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn synchronize() {
        {
            let ml = STS_LOCK.lock(false);
            SUSPEND_ALL.store(true, Ordering::Relaxed);

            if Self::is_synchronized() {
                return;
            }
            drop(ml); // Release lock before semaphore wait.
        }

        // Semaphore initial count is zero.  To reach here, there must be at
        // least one not yielded thread in the set, e.g. is_synchronized()
        // was false before the lock was released.  A thread in the set will
        // signal the semaphore iff it is the last to yield or leave while
        // there is an active suspend request.  So there will be exactly one
        // signal, which will increment the semaphore count to one, which
        // will then be consumed by this wait, returning it to zero.  No
        // thread can exit yield or enter the set until desynchronize is
        // called, so there are no further opportunities for the semaphore
        // being signaled until we get back here again for some later
        // synchronize call.  Hence, there is no need to re-check for
        // is_synchronized after the wait; it will always be true there.
        SYNCHRONIZE_WAKEUP.wait();
    }

    pub fn desynchronize() {
        let ml = STS_LOCK.lock(false);
        assert!(Self::suspend_all(), "STS not synchronizing");
        assert!(Self::is_synchronized(), "STS not synchronized");
        SUSPEND_ALL.store(false, Ordering::Relaxed);

        ml.notify_all();
    }
}

pub struct SuspendibleThreadSetJoiner {
    active: bool,
}

impl SuspendibleThreadSetJoiner {
    pub fn new(active: bool) -> Self {
        if active {
            SuspendibleThreadSet::join();
        }

        Self { active }
    }

    pub fn should_yield(&self) -> bool {
        self.active && SuspendibleThreadSet::should_yield()
    }

    pub fn yield_(self) {
        assert!(self.active, "Thread has not joined the suspendible thread set");
        SuspendibleThreadSet::yield_();
    }
}

impl Drop for SuspendibleThreadSetJoiner {
    fn drop(&mut self) {
        if self.active {
            SuspendibleThreadSet::leave();
        }
    }
}


pub struct SuspendibleThreadSetLeaver {
    active: bool 
}

impl SuspendibleThreadSetLeaver {
    pub fn new(active: bool) -> Self {
        if active {
            SuspendibleThreadSet::leave();
        }

        Self { active }
    }
}

impl Drop for SuspendibleThreadSetLeaver {
    fn drop(&mut self) {
        if self.active {
            SuspendibleThreadSet::join();
        }
    }
}