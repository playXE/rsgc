//! A mutual exclusion primitive useful for protecting shared data that is safe to use with RSGC.
//! 
//! API is entirely copied from parking_lot.

use crate::{
    heap::{safepoint, thread::Thread},
    thread::{safepoint_scope, safepoint_scope_conditional},
};
use parking_lot::lock_api;
use parking_lot_core::{
    deadlock, ParkResult, RequeueOp, SpinWait, UnparkResult, UnparkToken, DEFAULT_PARK_TOKEN,
};
use std::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicPtr, AtomicU8, Ordering},
    time::{Duration, Instant},
};

/// A type indicating whether a timed wait on a condition variable returned
/// due to a time out or not.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns whether the wait was known to have timed out.
    #[inline]
    pub fn timed_out(self) -> bool {
        self.0
    }
}

pub struct RawMutex {
    state: AtomicU8,
}

// UnparkToken used to indicate that that the target thread should attempt to
// lock the mutex again as soon as it is unparked.
pub(crate) const TOKEN_NORMAL: UnparkToken = UnparkToken(0);

// UnparkToken used to indicate that the mutex is being handed off to the target
// thread directly without unlocking it.
pub(crate) const TOKEN_HANDOFF: UnparkToken = UnparkToken(1);

/// This bit is set in the `state` of a `RawMutex` when that mutex is locked by some thread.
const LOCKED_BIT: u8 = 0b01;
/// This bit is set in the `state` of a `RawMutex` just before parking a thread. A thread is being
/// parked if it wants to lock the mutex, but it is currently being held by some other thread.
const PARKED_BIT: u8 = 0b10;

impl RawMutex {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
        }
    }

    #[inline]
    fn try_lock(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED_BIT != 0 {
                return false;
            }
            match self.state.compare_exchange_weak(
                state,
                state | LOCKED_BIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    unsafe { deadlock::acquire_resource(self as *const _ as usize) };
                    return true;
                }
                Err(x) => state = x,
            }
        }
    }

    #[inline]
    pub fn lock(&self, safepoint: bool) {
        if self
            .state
            .compare_exchange_weak(0, LOCKED_BIT, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_slow(None, safepoint);
        }
    }

    #[inline]
    pub fn unlock(&self) {
        if self
            .state
            .compare_exchange(LOCKED_BIT, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        self.unlock_slow(false);
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        state & LOCKED_BIT != 0
    }

    #[inline]
    pub fn unlock_fair(&self) {
        if self
            .state
            .compare_exchange(LOCKED_BIT, 0, Ordering::Release, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
        self.unlock_slow(true);
    }

    #[inline]
    pub fn bump(&self, safepoint: bool) {
        if self.state.load(Ordering::Relaxed) & PARKED_BIT != 0 {
            self.bump_slow(safepoint);
        }
    }

    #[cold]
    fn lock_slow(&self, timeout: Option<Instant>, safepoint: bool) -> bool {
        let mut spinwait = SpinWait::new();
        let mut state = self.state.load(Ordering::Relaxed);
        safepoint_scope_conditional(safepoint, || {
            loop {
                // Grab the lock if it isn't locked, even if there is a queue on it
                if state & LOCKED_BIT == 0 {
                    match self.state.compare_exchange_weak(
                        state,
                        state | LOCKED_BIT,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            return true;
                        }
                        Err(x) => state = x,
                    }
                    continue;
                }

                // If there is no queue, try spinning a few times
                if state & PARKED_BIT == 0 && spinwait.spin() {
                    state = self.state.load(Ordering::Relaxed);
                    continue;
                }

                // Set the parked bit
                if state & PARKED_BIT == 0 {
                    if let Err(x) = self.state.compare_exchange_weak(
                        state,
                        state | PARKED_BIT,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        state = x;
                        continue;
                    }
                }

                // Park our thread until we are woken up by an unlock
                let addr = self as *const _ as usize;
                let validate = || self.state.load(Ordering::Relaxed) == LOCKED_BIT | PARKED_BIT;
                let before_sleep = || {};
                let timed_out = |_, was_last_thread| {
                    // Clear the parked bit if we were the last parked thread
                    if was_last_thread {
                        self.state.fetch_and(!PARKED_BIT, Ordering::Relaxed);
                    }
                };
                // SAFETY:
                //   * `addr` is an address we control.
                //   * `validate`/`timed_out` does not panic or call into any function of `parking_lot`.
                //   * `before_sleep` does not call `park`, nor does it panic.
                match unsafe {
                    parking_lot_core::park(
                        addr,
                        validate,
                        before_sleep,
                        timed_out,
                        DEFAULT_PARK_TOKEN,
                        timeout,
                    )
                } {
                    // The thread that unparked us passed the lock on to us
                    // directly without unlocking it.
                    ParkResult::Unparked(TOKEN_HANDOFF) => {
                        return true;
                    }

                    // We were unparked normally, try acquiring the lock again
                    ParkResult::Unparked(_) => (),

                    // The validation function failed, try locking again
                    ParkResult::Invalid => (),

                    // Timeout expired
                    ParkResult::TimedOut => {
                        return false;
                    }
                }

                // Loop back and try locking again
                spinwait.reset();
                state = self.state.load(Ordering::Relaxed);
            }
        })
    }

    #[cold]
    fn unlock_slow(&self, force_fair: bool) {
        // Unpark one thread and leave the parked bit set if there might
        // still be parked threads on this address.
        let addr = self as *const _ as usize;
        let callback = |result: UnparkResult| {
            // If we are using a fair unlock then we should keep the
            // mutex locked and hand it off to the unparked thread.
            if result.unparked_threads != 0 && (force_fair || result.be_fair) {
                // Clear the parked bit if there are no more parked
                // threads.
                if !result.have_more_threads {
                    self.state.store(LOCKED_BIT, Ordering::Relaxed);
                }
                return TOKEN_HANDOFF;
            }

            // Clear the locked bit, and the parked bit as well if there
            // are no more parked threads.
            if result.have_more_threads {
                self.state.store(PARKED_BIT, Ordering::Release);
            } else {
                self.state.store(0, Ordering::Release);
            }
            TOKEN_NORMAL
        };
        // SAFETY:
        //   * `addr` is an address we control.
        //   * `callback` does not panic or call into any function of `parking_lot`.
        unsafe {
            parking_lot_core::unpark_one(addr, callback);
        }
    }

    #[cold]
    fn bump_slow(&self, safepoint: bool) {
        self.unlock_slow(true);
        self.lock(safepoint);
    }

    // Used by Condvar when requeuing threads to us, must be called while
    // holding the queue lock.
    #[inline]
    pub(crate) fn mark_parked_if_locked(&self) -> bool {
        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state & LOCKED_BIT == 0 {
                return false;
            }
            match self.state.compare_exchange_weak(
                state,
                state | PARKED_BIT,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(x) => state = x,
            }
        }
    }

    // Used by Condvar when requeuing threads to us, must be called while
    // holding the queue lock.
    #[inline]
    pub(crate) fn mark_parked(&self) {
        self.state.fetch_or(PARKED_BIT, Ordering::Relaxed);
    }
}

pub struct Mutex<T: ?Sized> {
    raw: RawMutex,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized> Send for Mutex<T> {}
unsafe impl<T: ?Sized> Sync for Mutex<T> {}

impl<T: ?Sized> Mutex<T> {
    pub unsafe fn unsafe_get(&self) -> &T {
        &*self.data.get()
    }

    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data.get() }
    }

    #[inline]
    pub const fn new(val: T) -> Mutex<T>
    where
        T: Sized,
    {
        Mutex {
            raw: RawMutex::new(),
            data: UnsafeCell::new(val),
        }
    }

    /// Returns the underlying raw mutex object.
    ///
    /// Note that you will most likely need to import the `RawMutex` trait from
    /// `lock_api` to be able to call functions on the raw mutex.
    ///
    /// # Safety
    ///
    /// This method is unsafe because it allows unlocking a mutex while
    /// still holding a reference to a `MutexGuard`.
    #[inline]
    pub unsafe fn raw(&self) -> &RawMutex {
        &self.raw
    }

    #[inline]
    pub fn lock(&self, safepoint: bool) -> MutexGuard<'_, T> {
        self.raw.lock(safepoint);
        unsafe { self.guard(safepoint) }
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned. The lock will be unlocked when the
    /// guard is dropped.
    ///
    /// This function does not block.
    #[inline]
    pub fn try_lock(&self, safepoint: bool) -> Option<MutexGuard<'_, T>> {
        if self.raw.try_lock() {
            // SAFETY: The lock is held, as required.
            Some(unsafe { self.guard(safepoint) })
        } else {
            None
        }
    }

    /// # Safety
    ///
    /// The lock must be held when calling this method.
    #[inline]
    unsafe fn guard(&self, safepoint: bool) -> MutexGuard<'_, T> {
        MutexGuard {
            mutex: self,
            safepoint,
        }
    }
}

pub struct MutexGuard<'a, T: ?Sized> {
    pub(crate) mutex: &'a Mutex<T>,
    safepoint: bool,
}

impl<'a, T: ?Sized> MutexGuard<'a, T> {
    /// Temporarily unlocks the mutex to execute the given function.
    ///
    /// This is safe because `&mut` guarantees that there exist no other
    /// references to the data protected by the mutex.
    #[inline]
    pub fn unlocked<F, U>(s: &mut Self, f: F) -> U
    where
        F: FnOnce() -> U,
    {
        // Safety: A MutexGuard always holds the lock.
        s.mutex.raw.unlock();

        let r = f();
        s.mutex.raw.lock(s.safepoint);
        r
    }

    /// Returns a reference to the original `Mutex` object.
    pub fn mutex(s: &Self) -> &'a Mutex<T> {
        s.mutex
    }
}

impl<'a, T: ?Sized> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<'a, T: ?Sized> Deref for MutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.data.get() }
    }
}

use std::ptr;

pub struct Condvar {
    state: AtomicPtr<RawMutex>,
}

impl Condvar {
    /// Creates a new condition variable which is ready to be waited on and
    /// notified.
    #[inline]
    pub const fn new() -> Condvar {
        Condvar {
            state: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Wakes up one blocked thread on this condvar.
    ///
    /// Returns whether a thread was woken up.
    ///
    /// If there is a blocked thread on this condition variable, then it will
    /// be woken up from its call to `wait` or `wait_timeout`. Calls to
    /// `notify_one` are not buffered in any way.
    ///
    /// To wake up all threads, see `notify_all()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use parking_lot::Condvar;
    ///
    /// let condvar = Condvar::new();
    ///
    /// // do something with condvar, share it with other threads
    ///
    /// if !condvar.notify_one() {
    ///     println!("Nobody was listening for this.");
    /// }
    /// ```
    #[inline]
    pub fn notify_one(&self) -> bool {
        // Nothing to do if there are no waiting threads
        let state = self.state.load(Ordering::Relaxed);
        if state.is_null() {
            return false;
        }

        self.notify_one_slow(state)
    }

    #[cold]
    fn notify_one_slow(&self, mutex: *mut RawMutex) -> bool {
        // Unpark one thread and requeue the rest onto the mutex
        let from = self as *const _ as usize;
        let to = mutex as usize;
        let validate = || {
            // Make sure that our atomic state still points to the same
            // mutex. If not then it means that all threads on the current
            // mutex were woken up and a new waiting thread switched to a
            // different mutex. In that case we can get away with doing
            // nothing.
            if self.state.load(Ordering::Relaxed) != mutex {
                return RequeueOp::Abort;
            }

            // Unpark one thread if the mutex is unlocked, otherwise just
            // requeue everything to the mutex. This is safe to do here
            // since unlocking the mutex when the parked bit is set requires
            // locking the queue. There is the possibility of a race if the
            // mutex gets locked after we check, but that doesn't matter in
            // this case.
            if unsafe { (*mutex).mark_parked_if_locked() } {
                RequeueOp::RequeueOne
            } else {
                RequeueOp::UnparkOne
            }
        };
        let callback = |_op, result: UnparkResult| {
            // Clear our state if there are no more waiting threads
            if !result.have_more_threads {
                self.state.store(ptr::null_mut(), Ordering::Relaxed);
            }
            TOKEN_NORMAL
        };
        let res = unsafe { parking_lot_core::unpark_requeue(from, to, validate, callback) };

        res.unparked_threads + res.requeued_threads != 0
    }

    /// Wakes up all blocked threads on this condvar.
    ///
    /// Returns the number of threads woken up.
    ///
    /// This method will ensure that any current waiters on the condition
    /// variable are awoken. Calls to `notify_all()` are not buffered in any
    /// way.
    ///
    /// To wake up only one thread, see `notify_one()`.
    #[inline]
    pub fn notify_all(&self) -> usize {
        // Nothing to do if there are no waiting threads
        let state = self.state.load(Ordering::Relaxed);
        if state.is_null() {
            return 0;
        }

        self.notify_all_slow(state)
    }

    #[cold]
    fn notify_all_slow(&self, mutex: *mut RawMutex) -> usize {
        // Unpark one thread and requeue the rest onto the mutex
        let from = self as *const _ as usize;
        let to = mutex as usize;
        let validate = || {
            // Make sure that our atomic state still points to the same
            // mutex. If not then it means that all threads on the current
            // mutex were woken up and a new waiting thread switched to a
            // different mutex. In that case we can get away with doing
            // nothing.
            if self.state.load(Ordering::Relaxed) != mutex {
                return RequeueOp::Abort;
            }

            // Clear our state since we are going to unpark or requeue all
            // threads.
            self.state.store(ptr::null_mut(), Ordering::Relaxed);

            // Unpark one thread if the mutex is unlocked, otherwise just
            // requeue everything to the mutex. This is safe to do here
            // since unlocking the mutex when the parked bit is set requires
            // locking the queue. There is the possibility of a race if the
            // mutex gets locked after we check, but that doesn't matter in
            // this case.
            if unsafe { (*mutex).mark_parked_if_locked() } {
                RequeueOp::RequeueAll
            } else {
                RequeueOp::UnparkOneRequeueRest
            }
        };
        let callback = |op, result: UnparkResult| {
            // If we requeued threads to the mutex, mark it as having
            // parked threads. The RequeueAll case is already handled above.
            if op == RequeueOp::UnparkOneRequeueRest && result.requeued_threads != 0 {
                unsafe { (*mutex).mark_parked() };
            }
            TOKEN_NORMAL
        };
        let res = unsafe { parking_lot_core::unpark_requeue(from, to, validate, callback) };

        res.unparked_threads + res.requeued_threads
    }

    /// Blocks the current thread until this condition variable receives a
    /// notification.
    ///
    /// This function will atomically unlock the mutex specified (represented by
    /// `mutex_guard`) and block the current thread. This means that any calls
    /// to `notify_*()` which happen logically after the mutex is unlocked are
    /// candidates to wake this thread up. When this function call returns, the
    /// lock specified will have been re-acquired.
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar`
    /// with a different `Mutex` object.
    #[inline]
    pub fn wait<T: ?Sized>(&self, mutex_guard: &mut MutexGuard<'_, T>) {
        self.wait_until_internal(
            mutex_guard.safepoint,
            unsafe { MutexGuard::mutex(mutex_guard).raw() },
            None,
        );
    }

    /// Waits on this condition variable for a notification, timing out after
    /// the specified time instant.
    ///
    /// The semantics of this function are equivalent to `wait()` except that
    /// the thread will be blocked roughly until `timeout` is reached. This
    /// method should not be used for precise timing due to anomalies such as
    /// preemption or platform differences that may not cause the maximum
    /// amount of time waited to be precisely `timeout`.
    ///
    /// Note that the best effort is made to ensure that the time waited is
    /// measured with a monotonic clock, and not affected by the changes made to
    /// the system time.
    ///
    /// The returned `WaitTimeoutResult` value indicates if the timeout is
    /// known to have elapsed.
    ///
    /// Like `wait`, the lock specified will be re-acquired when this function
    /// returns, regardless of whether the timeout elapsed or not.
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar`
    /// with a different `Mutex` object.
    #[inline]
    pub fn wait_until<T: ?Sized>(
        &self,
        mutex_guard: &mut MutexGuard<'_, T>,
        timeout: Instant,
    ) -> WaitTimeoutResult {
        let safepoint = mutex_guard.safepoint;
        self.wait_until_internal(
            safepoint,
            unsafe { MutexGuard::mutex(mutex_guard).raw() },
            Some(timeout),
        )
    }

    pub fn wait_raw(&self, safepoint: bool, mutex: &RawMutex) {
        self.wait_until_internal(safepoint, mutex, None);
    }

    // This is a non-generic function to reduce the monomorphization cost of
    // using `wait_until`.
    fn wait_until_internal(
        &self,
        safepoint: bool,
        mutex: &RawMutex,
        timeout: Option<Instant>,
    ) -> WaitTimeoutResult {
        let mut result = ParkResult::Invalid;
        let mut bad_mutex = false;
        let mut requeued = false;
        safepoint_scope_conditional(safepoint, || {
            {
                let addr = self as *const _ as usize;
                let lock_addr = mutex as *const _ as *mut _;
                let validate = || {
                    // Ensure we don't use two different mutexes with the same
                    // Condvar at the same time. This is done while locked to
                    // avoid races with notify_one
                    let state = self.state.load(Ordering::Relaxed);
                    if state.is_null() {
                        self.state.store(lock_addr, Ordering::Relaxed);
                    } else if state != lock_addr {
                        bad_mutex = true;
                        return false;
                    }
                    true
                };
                let before_sleep = || {
                    // Unlock the mutex before sleeping...
                    mutex.unlock();
                };
                let timed_out = |k, was_last_thread| {
                    // If we were requeued to a mutex, then we did not time out.
                    // We'll just park ourselves on the mutex again when we try
                    // to lock it later.
                    requeued = k != addr;

                    // If we were the last thread on the queue then we need to
                    // clear our state. This is normally done by the
                    // notify_{one,all} functions when not timing out.
                    if !requeued && was_last_thread {
                        self.state.store(ptr::null_mut(), Ordering::Relaxed);
                    }
                };
                result = unsafe {
                    parking_lot_core::park(
                        addr,
                        validate,
                        before_sleep,
                        timed_out,
                        DEFAULT_PARK_TOKEN,
                        timeout,
                    )
                };
            }

            // Panic if we tried to use multiple mutexes with a Condvar. Note
            // that at this point the MutexGuard is still locked. It will be
            // unlocked by the unwinding logic.
            if bad_mutex {
                panic!("attempted to use a condition variable with more than one mutex");
            }

            // ... and re-lock it once we are done sleeping
            if result == ParkResult::Unparked(TOKEN_HANDOFF) {
                unsafe { deadlock::acquire_resource(mutex as *const _ as usize) };
            } else {
                mutex.lock(safepoint);
            }
        });

        WaitTimeoutResult(!(result.is_unparked() || requeued))
    }

    /// Waits on this condition variable for a notification, timing out after a
    /// specified duration.
    ///
    /// The semantics of this function are equivalent to `wait()` except that
    /// the thread will be blocked for roughly no longer than `timeout`. This
    /// method should not be used for precise timing due to anomalies such as
    /// preemption or platform differences that may not cause the maximum
    /// amount of time waited to be precisely `timeout`.
    ///
    /// Note that the best effort is made to ensure that the time waited is
    /// measured with a monotonic clock, and not affected by the changes made to
    /// the system time.
    ///
    /// The returned `WaitTimeoutResult` value indicates if the timeout is
    /// known to have elapsed.
    ///
    /// Like `wait`, the lock specified will be re-acquired when this function
    /// returns, regardless of whether the timeout elapsed or not.
    #[inline]
    pub fn wait_for<T: ?Sized>(
        &self,
        mutex_guard: &mut MutexGuard<'_, T>,
        timeout: Duration,
    ) -> WaitTimeoutResult {
        let deadline = to_deadline(timeout);
        self.wait_until_internal(
            mutex_guard.safepoint,
            unsafe { MutexGuard::mutex(mutex_guard).raw() },
            deadline,
        )
    }

    #[inline]
    fn wait_while_until_internal<T, F>(
        &self,
        mutex_guard: &mut MutexGuard<'_, T>,
        mut condition: F,
        timeout: Option<Instant>,
    ) -> WaitTimeoutResult
    where
        T: ?Sized,
        F: FnMut(&mut T) -> bool,
    {
        let mut result = WaitTimeoutResult(false);

        while !result.timed_out() && condition(mutex_guard.deref_mut()) {
            result = self.wait_until_internal(
                mutex_guard.safepoint,
                unsafe { MutexGuard::mutex(mutex_guard).raw() },
                timeout,
            );
        }

        result
    }
    /// Blocks the current thread until this condition variable receives a
    /// notification. If the provided condition evaluates to `false`, then the
    /// thread is no longer blocked and the operation is completed. If the
    /// condition evaluates to `true`, then the thread is blocked again and
    /// waits for another notification before repeating this process.
    ///
    /// This function will atomically unlock the mutex specified (represented by
    /// `mutex_guard`) and block the current thread. This means that any calls
    /// to `notify_*()` which happen logically after the mutex is unlocked are
    /// candidates to wake this thread up. When this function call returns, the
    /// lock specified will have been re-acquired.
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar`
    /// with a different `Mutex` object.
    #[inline]
    pub fn wait_while<T, F>(&self, mutex_guard: &mut MutexGuard<'_, T>, condition: F)
    where
        T: ?Sized,
        F: FnMut(&mut T) -> bool,
    {
        self.wait_while_until_internal(mutex_guard, condition, None);
    }

    /// Waits on this condition variable for a notification, timing out after
    /// the specified time instant. If the provided condition evaluates to
    /// `false`, then the thread is no longer blocked and the operation is
    /// completed. If the condition evaluates to `true`, then the thread is
    /// blocked again and waits for another notification before repeating
    /// this process.
    ///
    /// The semantics of this function are equivalent to `wait()` except that
    /// the thread will be blocked roughly until `timeout` is reached. This
    /// method should not be used for precise timing due to anomalies such as
    /// preemption or platform differences that may not cause the maximum
    /// amount of time waited to be precisely `timeout`.
    ///
    /// Note that the best effort is made to ensure that the time waited is
    /// measured with a monotonic clock, and not affected by the changes made to
    /// the system time.
    ///
    /// The returned `WaitTimeoutResult` value indicates if the timeout is
    /// known to have elapsed.
    ///
    /// Like `wait`, the lock specified will be re-acquired when this function
    /// returns, regardless of whether the timeout elapsed or not.
    ///
    /// # Panics
    ///
    /// This function will panic if another thread is waiting on the `Condvar`
    /// with a different `Mutex` object.
    #[inline]
    pub fn wait_while_until<T, F>(
        &self,
        mutex_guard: &mut MutexGuard<'_, T>,
        condition: F,
        timeout: Instant,
    ) -> WaitTimeoutResult
    where
        T: ?Sized,
        F: FnMut(&mut T) -> bool,
    {
        self.wait_while_until_internal(mutex_guard, condition, Some(timeout))
    }

    /// Waits on this condition variable for a notification, timing out after a
    /// specified duration. If the provided condition evaluates to `false`,
    /// then the thread is no longer blocked and the operation is completed.
    /// If the condition evaluates to `true`, then the thread is blocked again
    /// and waits for another notification before repeating this process.
    ///
    /// The semantics of this function are equivalent to `wait()` except that
    /// the thread will be blocked for roughly no longer than `timeout`. This
    /// method should not be used for precise timing due to anomalies such as
    /// preemption or platform differences that may not cause the maximum
    /// amount of time waited to be precisely `timeout`.
    ///
    /// Note that the best effort is made to ensure that the time waited is
    /// measured with a monotonic clock, and not affected by the changes made to
    /// the system time.
    ///
    /// The returned `WaitTimeoutResult` value indicates if the timeout is
    /// known to have elapsed.
    ///
    /// Like `wait`, the lock specified will be re-acquired when this function
    /// returns, regardless of whether the timeout elapsed or not.
    #[inline]
    pub fn wait_while_for<T: ?Sized, F>(
        &self,
        mutex_guard: &mut MutexGuard<'_, T>,
        condition: F,
        timeout: Duration,
    ) -> WaitTimeoutResult
    where
        F: FnMut(&mut T) -> bool,
    {
        let deadline = to_deadline(timeout);
        self.wait_while_until_internal(mutex_guard, condition, deadline)
    }
}

#[inline]
pub fn to_deadline(timeout: Duration) -> Option<Instant> {
    Instant::now().checked_add(timeout)
}

impl<'a, T: ?Sized> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        unsafe {
            self.mutex.raw().unlock();
        }
    }
}
