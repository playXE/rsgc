use std::{time::Instant, ops::{Deref, DerefMut}};

use super::mutex::{Mutex, Condvar, MutexGuard, WaitTimeoutResult};


/// A monitor is a synchronization primitive that can be used to protect shared data from concurrent access.
/// 
/// It combines a mutex and a condition variable.
pub struct Monitor<T> {
    mutex: Mutex<T>,
    cv: Condvar
}

unsafe impl<T> Send for Monitor<T> {}
unsafe impl<T> Sync for Monitor<T> {}

impl<T> Monitor<T> {
    pub const fn new(val: T) -> Self {
        Self {
            mutex: Mutex::new(val),
            cv: Condvar::new()
        }
    }

    pub fn lock<'a>(&'a self, safepoint: bool) -> MonitorLocker<'a, T> {
        MonitorLocker {
            guard: self.mutex.lock(safepoint),
            cv: &self.cv
        }
    }

    pub fn try_lock<'a>(&'a self, safepoint: bool) -> Option<MonitorLocker<'a, T>> {
        self.mutex.try_lock(safepoint).map(|guard| MonitorLocker {
            guard,
            cv: &self.cv
        })
    }

    pub fn notify_all(&self) -> usize {
        self.cv.notify_all()
    }

    pub fn notify_one(&self) -> bool {
        self.cv.notify_one()
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.mutex.get_mut()
    }

    pub unsafe fn unsafe_get(&self) -> &T {
        self.mutex.unsafe_get()
    }

    pub fn mutex(&self) -> &Mutex<T> {
        &self.mutex
    }

    


}

pub struct MonitorLocker<'a, T> {
    cv: &'a Condvar,
    guard: MutexGuard<'a, T>,
}

impl<'a, T> MonitorLocker<'a, T> {
    pub fn wait(&mut self) {
        self.cv.wait(&mut self.guard);
    }

    pub fn wait_until(&mut self, timeout: Instant) {
        self.cv.wait_until(&mut self.guard, timeout);
    }

    pub fn wait_for(&mut self, timeout: std::time::Duration) -> WaitTimeoutResult {
        self.cv.wait_for(&mut self.guard, timeout)
    }

    pub fn wait_while(&mut self, condition: impl FnMut(&mut T) -> bool) {
        self.cv.wait_while(&mut self.guard, condition)
    }

    pub fn notify(self) -> bool {
        let res = self.cv.notify_one();
        res 
    }

    pub fn notify_all(&self) -> usize {
        let res = self.cv.notify_all();
       
        res 
    }
}

impl<'a, T> Deref for MonitorLocker<'a, T> {
    type Target = MutexGuard<'a, T>;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, T> DerefMut for MonitorLocker<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
impl<'a, T> Drop for MonitorLocker<'a, T> {
    fn drop(&mut self) {
        self.cv.notify_all();
    }
}