use parking_lot::{lock_api::RawMutex, Condvar, Mutex};
use std::{
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicU32, AtomicU8, Ordering},
};
#[cfg(target_family = "windows")]
use winapi::um::{memoryapi::*, winnt::*};

use super::signals::install_signal_handlers;

pub(crate) static SAFEPOINT_PAGE: AtomicPtr<u8> = AtomicPtr::new(null_mut());
pub(crate) static SAFEPOINT_ENABLE_CNT: AtomicU8 = AtomicU8::new(0);

pub(crate) static SAFEPOINT_LOCK: Mutex<()> = Mutex::const_new(parking_lot::RawMutex::INIT, ());
pub(crate) static SAFEPOINT_COND: Condvar = Condvar::new();
pub(crate) static GC_RUNNING: AtomicU32 = AtomicU32::new(0);

pub fn addr_in_safepoint(addr: usize) -> bool {
    let page = SAFEPOINT_PAGE.load(Ordering::Relaxed);
    if page.is_null() {
        // ?? unreachable actually
        return false;
    }
    let page = page as usize;
    addr >= page && addr < page + 4096
}

pub fn enable() {
    if SAFEPOINT_ENABLE_CNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) != 0 {
        return;
    }

    let pageaddr = SAFEPOINT_PAGE.load(std::sync::atomic::Ordering::Relaxed);
    unsafe {
        #[cfg(not(windows))]
        {
            libc::mprotect(
                pageaddr as _,
                super::virtual_memory::page_size(),
                libc::PROT_NONE,
            );
        }
        #[cfg(windows)]
        {
            let mut odl_prot = 0;
            VirtualProtect(
                pageaddr as _,
                super::virtual_memory::page_size() as _,
                PAGE_NOACCESS,
                &mut old_prot,
            );
        }
    }
}
pub fn disable() {
    if SAFEPOINT_ENABLE_CNT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) - 1 != 0 {
        return;
    }

    let pageaddr = SAFEPOINT_PAGE.load(std::sync::atomic::Ordering::Relaxed);

    unsafe {
        #[cfg(not(windows))]
        {
            libc::mprotect(
                pageaddr as _,
                super::virtual_memory::page_size(),
                libc::PROT_READ,
            );
        }
        #[cfg(windows)]
        {
            let mut odl_prot = 0;
            VirtualProtect(
                pageaddr as _,
                super::virtual_memory::page_size() as _,
                PAGE_READONLY,
                &mut old_prot,
            );
        }
    }
}

pub fn init() {
    unsafe {
        let pgsz = super::virtual_memory::page_size();
        let mut addr;
        cfg_if::cfg_if! {
            if #[cfg(windows)] {
                addr = VirtualProtect(core::ptr::null_mut(), pgsz, MEM_COMMIT, PAGE_READONLY) as *mut u8;
                addr = addr;
            } else {
                addr = libc::mmap(null_mut(), pgsz, libc::PROT_READ, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) as *mut u8;
                if addr == libc::MAP_FAILED as *mut u8 {
                    addr = null_mut();
                }
            }
        };

        if addr.is_null() {
            panic!("could not allocate GC synchronization page");
        }

        SAFEPOINT_PAGE.store(addr, std::sync::atomic::Ordering::Relaxed);
    }
    install_signal_handlers();
}

pub fn enter() -> bool {
    let guard = SAFEPOINT_LOCK.lock();

    match GC_RUNNING.compare_exchange(0, 1, Ordering::Relaxed, Ordering::SeqCst) {
        Ok(_) => {
            enable();
            drop(guard);

            true
        }

        Err(_) => {
            // In case multiple threads enter the GC at the same time, only allow
            // one of them to actually run the collection. We can't just let the
            // master thread do the GC since it might be running unmanaged code
            // and can take arbitrarily long time before hitting a safe point.
            drop(guard);
            wait_gc();
            return false;
        }
    }
}

pub fn end() {
    let guard = SAFEPOINT_LOCK.lock();

    disable();
    GC_RUNNING.store(0, Ordering::Release);
    drop(guard);
    SAFEPOINT_COND.notify_all();
}

pub fn wait_gc() {
    while GC_RUNNING.load(Ordering::Relaxed) != 0 || GC_RUNNING.load(Ordering::Acquire) != 0 {
        let mut guard = SAFEPOINT_LOCK.lock();
        if GC_RUNNING.load(Ordering::Relaxed) != 0 {
            SAFEPOINT_COND.wait(&mut guard);
        }

        drop(guard);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc, Barrier,
    };

    use crate::heap::thread::{thread, wait_for_the_world};

    #[test]
    fn test_safepoints() {
        super::init();
        env_logger::init();

        static SPAWNED: AtomicUsize = AtomicUsize::new(0);
        static STOP: AtomicBool = AtomicBool::new(false);
        let handles = (0..8)
            .map(|_| {
                
                std::thread::spawn(move || {
                    let _ = thread();
                    let mut prev = SPAWNED.load(std::sync::atomic::Ordering::Acquire);
                    loop {
                        match SPAWNED.compare_exchange_weak(
                            prev,
                            prev + 1,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(err) => {
                                prev = err;
                            }
                        }
                    }

                    loop {
                        thread().safepoint();

                        if STOP.load(std::sync::atomic::Ordering::Acquire) {
                            
                            break;
                        }
                    }
                })
            })
            .collect::<Vec<_>>();

        while SPAWNED.load(std::sync::atomic::Ordering::SeqCst) != 8 {}

        {
            assert!(super::enter());
            let th = unsafe { wait_for_the_world() };
            assert!(th.len() == 8);
            super::end();
            STOP.store(true, std::sync::atomic::Ordering::Release);
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
