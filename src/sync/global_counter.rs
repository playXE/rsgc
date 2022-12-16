
/// The GlobalCounter provides a synchronization mechanism between threads for
/// safe memory reclamation and other ABA problems. All readers must call
/// critical_section_begin before reading the volatile data and
/// critical_section_end afterwards. Such read-side critical sections may
/// be properly nested. The write side must call write_synchronize
/// before reclaming the memory. The read-path only does an uncontended store
/// to a thread-local-storage and fence to stop any loads from floating up, thus
/// light weight and wait-free. The write-side is more heavy since it must check
/// all readers and wait until they have left the generation. (a system memory
/// barrier can be used on write-side to remove fence in read-side,
/// not implemented).
pub struct GlobalCounter {}

use std::marker::PhantomData;

use parking_lot::{RawRwLock as RwLock, lock_api::RawRwLock};

static GLOBAL_COUNTER: RwLock = RwLock::INIT;

impl GlobalCounter {
    pub unsafe fn critical_section_begin() {
        GLOBAL_COUNTER.lock_shared();
    }

    pub unsafe fn critical_section_end() {
        GLOBAL_COUNTER.unlock_shared();
    }

    pub fn write_synchronize<F,R>(f: F) -> R 
    where F: FnOnce() -> R
    {
        GLOBAL_COUNTER.lock_exclusive();
        let ret = f();
        unsafe { GLOBAL_COUNTER.unlock_exclusive(); }
        ret
    }
}

pub struct CriticalSection {
    marker: PhantomData<*mut u8>
}


impl CriticalSection {
    pub fn new() -> Self {
        unsafe {
            GlobalCounter::critical_section_begin();
        }
        Self {
            marker: PhantomData
        }
    }
}

impl Drop for CriticalSection {
    fn drop(&mut self) {
        unsafe {
            GlobalCounter::critical_section_end();
        }
    }
}