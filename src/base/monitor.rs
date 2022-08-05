use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use parking_lot::{lock_api::RawMutex, Condvar, Mutex, MutexGuard, WaitTimeoutResult};

pub struct Monitor<T> {
    lock: Mutex<T>,
    condvar: Condvar,
}

impl<T> Monitor<T> {
    pub fn new(val: T) -> Self {
        Self {
            lock: Mutex::new(val),
            condvar: Condvar::new(),
        }
    }
    pub fn enter(&self) {
        unsafe {
            self.lock.raw().lock();
        }
    }

    pub unsafe fn data(&self) -> &T {
        &*self.lock.data_ptr()
    }

    pub unsafe fn data_mut(&self) -> &mut T {
        &mut *self.lock.data_ptr()
    }

    pub fn is_locked(&self) -> bool {
        self.lock.is_locked()
    }

    pub fn exit(&self) {
        unsafe {
            self.lock.raw().unlock();
        }
    }

    pub fn notify(&self) {
        self.condvar.notify_one();
    }

    pub fn notify_all(&self) {
        self.condvar.notify_all();
    }
}

pub struct MonitorLock<'a, T> {
    lock: MutexGuard<'a, T>,
    condvar: &'a Condvar,
}

impl<'a, T> MonitorLock<'a, T> {
    pub fn new(monitor: &'a Monitor<T>) -> Self {
        Self {
            lock: monitor.lock.lock(),
            condvar: &monitor.condvar,
        }
    }
    pub fn wait(&mut self) {
        self.condvar.wait(&mut self.lock);
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> WaitTimeoutResult {
        self.condvar.wait_for(&mut self.lock, timeout)
    }

    pub fn notify_one(&self) {
        self.condvar.notify_one();
    }

    pub fn notify_all(&self) {
        self.condvar.notify_all();
    }
}

impl<'a, T> Deref for MonitorLock<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.lock
    }
}

impl<'a, T> DerefMut for MonitorLock<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.lock
    }
}
