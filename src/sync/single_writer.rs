use super::semaphore::Sem;
use atomic::Ordering;
use std::sync::atomic::AtomicUsize;

///  Synchronization primitive inspired by RCU.
///
///  Any number of threads may enter critical sections associated with a
///  synchronizer object.  One (at a time) other thread may wait for the
///  completion of all critical sections for the synchronizer object
///  that were extant when the wait was initiated.  Usage is that there
///  is some state that can be accessed either before or after some
///  change.  An accessing thread performs the access within a critical
///  section.  A writer thread performs the state change, and then waits
///  for critical sections to complete, thereby ensuring there are no
///  threads in a critical section that might have seen the old state.
///
///  Generally, GlobalCounter should be used instead of this class, as
///  GlobalCounter has measurably better performance and doesn't have
///  the single writer at a time restriction.  Use this only in
///  situations where GlobalCounter won't work for some reason.
pub struct SingleWriterSynchronizer {
    enter: AtomicUsize,
    exit: [AtomicUsize; 2],
    waiting_for: AtomicUsize,
    wakeup: Sem,
}

impl SingleWriterSynchronizer {
    #[inline]
    pub fn enter(&self) -> usize {
        self.enter.fetch_add(2, Ordering::Relaxed) + 2
    }

    #[inline]
    pub fn exit(&self, enter_value: usize) {
        let exit_value = self.exit[enter_value & 1].fetch_add(2, Ordering::Relaxed) + 2;
        if exit_value == self.waiting_for.load(Ordering::Relaxed) {
            self.wakeup.signal(1);
        }
    }

    pub fn synchronize(&self) {
        // We don't know anything about the muxing between this invocation
        // and invocations in other threads.  We must start with the latest
        // enter polarity, else we could clobber the wrong exit value on
        // the first iteration.  So fence to ensure everything here follows
        // whatever muxing was used.
        atomic::fence(Ordering::SeqCst);

        let mut value = self.enter.load(Ordering::Relaxed);
        // (1) Determine the old and new exit counters, based on the
        // polarity (bit0 value) of the on-entry enter counter.
        let new_ptr = &self.exit[(value + 1) & 1];

        // (2) Change the in-use exit counter to the new counter, by adding
        // 1 to the enter counter (flipping the polarity), meanwhile
        // "simultaneously" initializing the new exit counter to that enter
        // value.  Note: The new exit counter is not being used by read
        // operations until this change of _enter succeeds.
        let mut old;

        loop {
            old = value;
            value += 1;
            new_ptr.store(value, Ordering::Relaxed);
            match self
                .enter
                .compare_exchange_weak(old, value, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(v) => value = v,
            }
        }
        // Critical sections entered before we changed the polarity will use
        // the old exit counter.  Critical sections entered after the change
        // will use the new exit counter.
        let old_ptr = &self.exit[old & 1];
        // (3) Inform threads in in-progress critical sections that there is
        // a pending synchronize waiting.  The thread that completes the
        // request (_exit value == old) will signal the _wakeup semaphore to
        // allow us to proceed.
        self.waiting_for.store(old, Ordering::Relaxed);

        // Write of _waiting_for must precede read of _exit and associated
        // conditional semaphore wait.  If they were re-ordered then a
        // critical section exit could miss the wakeup request, failing to
        // signal us while we're waiting.
        atomic::fence(Ordering::SeqCst);

        while old != old_ptr.load(Ordering::Acquire) {
            self.wakeup.wait();
        }

        while self.wakeup.try_wait() {}
    }

    pub fn new() -> Self {
        Self {
            enter: AtomicUsize::new(0),
            exit: [AtomicUsize::new(0), AtomicUsize::new(0)],
            waiting_for: AtomicUsize::new(1),
            wakeup: Sem::new(0).unwrap(),
        }
    }
}


pub struct CriticalSection<'a> {
    synchronizer: &'a SingleWriterSynchronizer,
    enter_value: usize 
}

impl<'a> CriticalSection<'a> {
    pub fn new(synchronizer: &'a SingleWriterSynchronizer) -> Self {
        Self {
            synchronizer,
            enter_value: synchronizer.enter(),
        }
    }
}

impl<'a> Drop for CriticalSection<'a> {
    fn drop(&mut self) {
        self.synchronizer.exit(self.enter_value);
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::{Arc, atomic::AtomicI32}, time::Duration};

    use super::*;

    const NREADERS: usize = 5;
    #[test]
    fn test_single_writer_synchronizer() {
        let synchronizer = SingleWriterSynchronizer::new();
        let synchronized_value = AtomicUsize::new(0);
        let continue_running = AtomicI32::new(1);
        std::thread::scope(|scope| {
            let readers = (0..NREADERS).map(|_| {
                scope.spawn(|| {
                    let mut iterations = 0;
                    let mut values_changed = 0;

                    while continue_running.load(Ordering::Acquire) != 0 {
                        iterations += 1;
                        let cs = CriticalSection::new(&synchronizer);
                        let value = synchronized_value.load(Ordering::Acquire);
                        let mut new_value = value;

                        for _ in 0..10 {
                            new_value = synchronized_value.load(Ordering::Acquire);
                            if value != new_value {
                                assert_eq!(value + 1, new_value);
                            }
                        }

                        if value != new_value {
                            values_changed += 1;
                        }

                        drop(cs);
                    }

                    println!("reader iterations: {}, changes: {}", iterations, values_changed);
                })
            }).collect::<Vec<_>>();

            let writer = scope.spawn(|| {
                while continue_running.load(Ordering::Acquire) != 0 {
                    synchronized_value.fetch_add(1, Ordering::Relaxed);
                    synchronizer.synchronize();
                }

                println!("writer iterations: {}", synchronized_value.load(Ordering::Acquire));
            });

            std::thread::sleep(Duration::from_millis(1000));
            continue_running.store(0, Ordering::Release);
            for reader in readers {
                reader.join().unwrap();
            }

            writer.join().unwrap();
        });
    }
}