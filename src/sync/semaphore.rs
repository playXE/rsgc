mod os {
    #[cold]
    #[inline(never)]
    fn unlikely<T>(result: T) -> T {
        result
    }
    #[cfg(any(
        all(unix, not(any(target_os = "macos", target_os = "ios"))),
        target_os = "fuchsia"
    ))]
    pub mod posix {
        use super::unlikely;
        use core::cell::UnsafeCell;
        use core::mem;
        use core::sync::atomic::{AtomicU8, Ordering};
        use error_code::PosixError;

        const UNINIT: u8 = 0;
        const INITING: u8 = 0b01;
        const INITED: u8 = 0b10;

        ///POSIX implementation of Semaphore
        pub struct Sem {
            handle: UnsafeCell<mem::MaybeUninit<libc::sem_t>>,
            state: AtomicU8,
        }

        impl Sem {
            ///Creates new uninit instance.
            ///
            ///It is UB to use it until `init` is called.
            pub const unsafe fn new_uninit() -> Self {
                Self {
                    handle: UnsafeCell::new(mem::MaybeUninit::uninit()),
                    state: AtomicU8::new(UNINIT),
                }
            }

            #[inline(always)]
            ///Returns whether semaphore is successfully initialized
            pub fn is_init(&self) -> bool {
                self.state.load(Ordering::Acquire) == INITED
            }

            #[cold]
            #[inline(never)]
            fn await_init(&self) {
                //Wait for initialization to finish
                while self.state.load(Ordering::Acquire) == INITING {
                    core::hint::spin_loop();
                }
            }

            #[must_use]
            ///Initializes semaphore with provided `init` as initial value.
            ///
            ///Returns `true` on success.
            ///
            ///Returns `false` if semaphore is already initialized or initialization failed.
            pub fn init(&self, init: u32) -> bool {
                if let Ok(UNINIT) = self.state.compare_exchange(
                    UNINIT,
                    INITING,
                    Ordering::SeqCst,
                    Ordering::Acquire,
                ) {
                    let res = unsafe { libc::sem_init(self.handle.get() as _, 0, init as _) };

                    let res = match res {
                        0 => {
                            self.state.store(INITED, Ordering::Release);
                            true
                        }
                        _ => {
                            //TODO: assert against?
                            self.state.store(UNINIT, Ordering::Release);
                            false
                        }
                    };

                    unlikely(res)
                } else {
                    //Similarly to `Once` we give priority to already-init path
                    //although we do need to make sure it is finished
                    if self.state.load(Ordering::Acquire) != INITED {
                        self.await_init();
                    }

                    false
                }
            }

            ///Creates new instance, initializing it with `init`
            pub fn new(init: u32) -> Option<Self> {
                let result = unsafe { Self::new_uninit() };

                if result.init(init) {
                    Some(result)
                } else {
                    unlikely(None)
                }
            }

            ///Decrements self, returning immediately if it was signaled.
            ///
            ///Otherwise awaits for signal.
            pub fn wait(&self) {
                loop {
                    let res = unsafe { libc::sem_wait(mem::transmute(self.handle.get())) };

                    if res == -1 {
                        let errno = PosixError::last();
                        debug_assert_eq!(errno.raw_code(), libc::EINTR, "Unexpected error");
                        continue;
                    }

                    break;
                }
            }

            #[inline]
            ///Attempts to decrement self, returning whether self was signaled or not.
            ///
            ///Returns `true` if self was signaled.
            ///
            ///Returns `false` otherwise.
            pub fn try_wait(&self) -> bool {
                loop {
                    let res = unsafe { libc::sem_trywait(mem::transmute(self.handle.get())) };

                    if res == -1 {
                        let errno = PosixError::last();
                        if errno.is_would_block() {
                            break false;
                        }

                        debug_assert_eq!(errno.raw_code(), libc::EINTR, "Unexpected error");
                        continue;
                    }

                    break true;
                }
            }

            ///Attempts to decrement self within provided time, returning whether self was signaled or not.
            ///
            ///Returns `true` if self was signaled within specified timeout
            ///
            ///Returns `false` otherwise
            pub fn wait_timeout(&self, duration: core::time::Duration) -> bool {
                let mut timeout = mem::MaybeUninit::uninit();
                if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, timeout.as_mut_ptr()) } == -1
                {
                    panic!("Failed to get current time");
                }

                let mut timeout = unsafe { timeout.assume_init() };
                timeout.tv_sec = timeout.tv_sec.saturating_add(duration.as_secs() as _);
                timeout.tv_nsec = timeout.tv_nsec.saturating_add(duration.subsec_nanos() as _);
                if timeout.tv_nsec > 999999999 {
                    timeout.tv_nsec = 0;
                    timeout.tv_sec = timeout.tv_sec.saturating_add(1);
                }

                loop {
                    let res =
                        unsafe { libc::sem_timedwait(mem::transmute(self.handle.get()), &timeout) };

                    if res == -1 {
                        let errno = PosixError::last();
                        if errno.is_would_block() || errno.raw_code() == libc::ETIMEDOUT {
                            break false;
                        }

                        if errno.raw_code() != libc::EINTR {
                            panic!("Unexpected error: {}", errno);
                        }
                        continue;
                    }

                    break true;
                }
            }

            ///Increments self, waking any awaiting thread as result.
            pub fn signal(&self, count: usize) {
                for _ in 0..count {
                    let res = unsafe { libc::sem_post(mem::transmute(self.handle.get())) };
                    debug_assert_eq!(res, 0);
                }
            }

            ///Performs deinitialization.
            ///
            ///Using `Sem` after `close` is undefined behaviour, unless `init` is called
            pub unsafe fn close(&self) {
                let handle = self.handle.get();
                if let Ok(INITED) =
                    self.state
                        .compare_exchange(INITED, UNINIT, Ordering::SeqCst, Ordering::Acquire)
                {
                    libc::sem_destroy(mem::transmute(handle));
                }
            }
        }

        impl Drop for Sem {
            fn drop(&mut self) {
                unsafe {
                    self.close();
                }
            }
        }

        unsafe impl Send for Sem {}
        unsafe impl Sync for Sem {}
    }

    #[cfg(any(
        all(unix, not(any(target_os = "macos", target_os = "ios"))),
        target_os = "fuchsia"
    ))]
    pub use posix::*;

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos"
    ))]
    pub mod mach {
        use super::unlikely;
        use core::ffi::c_void;
        use core::sync::atomic::{AtomicPtr, Ordering};
        use core::{mem, ptr};

        #[repr(C)]
        struct TimeSpec {
            tv_sec: libc::c_uint,
            tv_nsec: libc::c_int,
        }

        impl Into<TimeSpec> for core::time::Duration {
            fn into(self) -> TimeSpec {
                use core::convert::TryFrom;

                TimeSpec {
                    tv_sec: libc::c_uint::try_from(self.as_secs())
                        .unwrap_or(libc::c_uint::max_value()),
                    tv_nsec: libc::c_int::try_from(self.subsec_nanos())
                        .unwrap_or(libc::c_int::max_value()),
                }
            }
        }

        const KERN_OPERATION_TIMED_OUT: libc::c_int = 49;
        const SYNC_POLICY_FIFO: libc::c_int = 0;

        extern "C" {
            static mach_task_self_: libc::c_uint;

            //typedef struct semaphore *semaphore_t;
            //Function takes semaphore_t*
            fn semaphore_create(
                task: libc::c_uint,
                semaphore: *mut *mut c_void,
                policy: libc::c_int,
                value: libc::c_int,
            ) -> libc::c_int;
            fn semaphore_signal(semaphore: *mut c_void) -> libc::c_int;
            fn semaphore_wait(semaphore: *mut c_void) -> libc::c_int;
            fn semaphore_timedwait(semaphore: *mut c_void, timeout: TimeSpec) -> libc::c_int;
            fn semaphore_destroy(task: libc::c_uint, semaphore: *mut c_void) -> libc::c_int;
        }

        ///MacOS semaphore based on mach API
        pub struct Sem {
            handle: AtomicPtr<c_void>,
        }

        impl Sem {
            ///Creates new uninit instance.
            ///
            ///It is UB to use it until `init` is called.
            pub const unsafe fn new_uninit() -> Self {
                Self {
                    handle: AtomicPtr::new(ptr::null_mut()),
                }
            }

            #[inline(always)]
            ///Returns whether semaphore is successfully initialized
            pub fn is_init(&self) -> bool {
                !self.handle.load(Ordering::Acquire).is_null()
            }

            #[must_use]
            ///Initializes semaphore with provided `init` as initial value.
            ///
            ///Returns `true` on success.
            ///
            ///Returns `false` if semaphore is already initialized or initialization failed.
            pub fn init(&self, init: u32) -> bool {
                if !self.handle.load(Ordering::Acquire).is_null() {
                    //Similarly to `Once` we give priority to already-init path
                    return false;
                } else {
                    let mut handle = mem::MaybeUninit::uninit();

                    let res = unsafe {
                        semaphore_create(
                            mach_task_self_,
                            handle.as_mut_ptr(),
                            SYNC_POLICY_FIFO,
                            init as libc::c_int,
                        )
                    };

                    let res = match res {
                        0 => unsafe {
                            let handle = handle.assume_init();
                            match self.handle.compare_exchange(
                                ptr::null_mut(),
                                handle,
                                Ordering::SeqCst,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => true,
                                Err(_) => {
                                    semaphore_destroy(mach_task_self_, handle);
                                    false
                                }
                            }
                        },
                        _ => false,
                    };

                    unlikely(res)
                }
            }

            ///Creates new instance, initializing it with `init`
            pub fn new(init: u32) -> Option<Self> {
                let result = unsafe { Self::new_uninit() };

                if result.init(init) {
                    Some(result)
                } else {
                    unlikely(None)
                }
            }

            ///Decrements self, returning immediately if it was signaled.
            ///
            ///Otherwise awaits for signal.
            pub fn wait(&self) {
                loop {
                    let result = unsafe { semaphore_wait(self.handle.load(Ordering::Acquire)) };
                    if result != libc::KERN_ABORTED {
                        assert_eq!(result, libc::KERN_SUCCESS, "Failed to wait on semaphore");
                        break;
                    }
                }
            }

            #[inline]
            ///Attempts to decrement self, returning whether self was signaled or not.
            ///
            ///Returns `true` if self was signaled.
            ///
            ///Returns `false` otherwise.
            pub fn try_wait(&self) -> bool {
                self.wait_timeout(core::time::Duration::from_secs(0))
            }

            ///Attempts to decrement self within provided time, returning whether self was signaled or not.
            ///
            ///Returns `true` if self was signaled within specified timeout
            ///
            ///Returns `false` otherwise
            pub fn wait_timeout(&self, timeout: core::time::Duration) -> bool {
                let result = unsafe {
                    semaphore_timedwait(self.handle.load(Ordering::Acquire), timeout.into())
                };

                debug_assert!(
                    result == 0 || result == KERN_OPERATION_TIMED_OUT,
                    "semaphore_timedwait() failed"
                );
                result == 0
            }

            ///Increments self, waking any awaiting thread as result.
            pub fn signal(&self, count: usize) {
                for _ in 0..count {
                    let res = unsafe { semaphore_signal(self.handle.load(Ordering::Acquire)) };

                    debug_assert_eq!(res, 0, "semaphore_signal() failed");
                }
            }

            ///Performs deinitialization.
            ///
            ///Using `Sem` after `close` is undefined behaviour, unless `init` is called
            pub unsafe fn close(&self) {
                let handle = self.handle.swap(ptr::null_mut(), Ordering::AcqRel);
                if !handle.is_null() {
                    semaphore_destroy(mach_task_self_, handle);
                }
            }
        }

        impl Drop for Sem {
            fn drop(&mut self) {
                unsafe {
                    self.close();
                }
            }
        }

        unsafe impl Send for Sem {}
        unsafe impl Sync for Sem {}
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos"
    ))]
    pub use mach::*;

    #[cfg(windows)]
    pub mod windows {
        use core::ffi::c_void;
        use core::ptr;
        use core::sync::atomic::{AtomicPtr, Ordering};

        use super::unlikely;

        const WAIT_OBJECT_0: u32 = 0;
        const WAIT_TIMEOUT: u32 = 0x00000102;
        const INFINITE: u32 = 0xFFFFFFFF;

        extern "system" {
            fn CloseHandle(handle: *mut c_void) -> i32;
            fn CreateSemaphoreW(
                attrs: *mut c_void,
                initial: i32,
                max: i32,
                name: *const u16,
            ) -> *mut c_void;
            fn WaitForSingleObject(handle: *mut c_void, timeout_ms: u32) -> u32;
            fn ReleaseSemaphore(
                handle: *mut c_void,
                increment: i32,
                previous_increment: *mut i32,
            ) -> i32;
        }

        ///Windows implementation of Semaphore
        pub struct Sem {
            handle: AtomicPtr<c_void>,
        }

        impl Sem {
            ///Creates new uninit instance.
            ///
            ///It is UB to use it until `init` is called.
            pub const unsafe fn new_uninit() -> Self {
                Self {
                    handle: AtomicPtr::new(ptr::null_mut()),
                }
            }

            #[inline(always)]
            ///Returns whether semaphore is successfully initialized
            pub fn is_init(&self) -> bool {
                !self.handle.load(Ordering::Acquire).is_null()
            }

            #[must_use]
            ///Initializes semaphore with provided `init` as initial value.
            ///
            ///Returns `true` on success.
            ///
            ///Returns `false` if semaphore is already initialized or initialization failed.
            pub fn init(&self, init: u32) -> bool {
                if !self.handle.load(Ordering::Acquire).is_null() {
                    //Similarly to `Once` we give priority to already-init path
                    return false;
                } else {
                    let handle = unsafe {
                        CreateSemaphoreW(
                            ptr::null_mut(),
                            init as i32,
                            i32::max_value(),
                            ptr::null(),
                        )
                    };

                    let res = match self.handle.compare_exchange(
                        ptr::null_mut(),
                        handle,
                        Ordering::SeqCst,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => !handle.is_null(),
                        Err(_) => {
                            unsafe {
                                CloseHandle(handle);
                            }
                            unlikely(false)
                        }
                    };

                    unlikely(res)
                }
            }

            ///Creates new instance, initializing it with `init`
            pub fn new(init: u32) -> Option<Self> {
                let result = unsafe { Self::new_uninit() };

                if result.init(init) {
                    Some(result)
                } else {
                    unlikely(None)
                }
            }

            ///Decrements self, returning immediately if it was signaled.
            ///
            ///Otherwise awaits for signal.
            pub fn wait(&self) {
                let result =
                    unsafe { WaitForSingleObject(self.handle.load(Ordering::Acquire), INFINITE) };

                match result {
                    WAIT_OBJECT_0 => (),
                    //We cannot really timeout when there is no timeout
                    other => panic!("Unexpected result: {}", other),
                }
            }

            #[inline]
            ///Attempts to decrement self, returning whether self was signaled or not.
            ///
            ///Returns `true` if self was signaled.
            ///
            ///Returns `false` otherwise.
            pub fn try_wait(&self) -> bool {
                self.wait_timeout(core::time::Duration::from_secs(0))
            }

            ///Attempts to decrement self within provided time, returning whether self was signaled or not.
            ///
            ///Returns `true` if self was signaled within specified timeout
            ///
            ///Returns `false` otherwise
            pub fn wait_timeout(&self, timeout: core::time::Duration) -> bool {
                use core::convert::TryInto;

                let result = unsafe {
                    WaitForSingleObject(
                        self.handle.load(Ordering::Acquire),
                        timeout.as_millis().try_into().unwrap_or(u32::max_value()),
                    )
                };

                match result {
                    WAIT_OBJECT_0 => true,
                    WAIT_TIMEOUT => false,
                    other => panic!("Unexpected result: {}", other),
                }
            }

            ///Increments self, waking any awaiting thread as result.
            pub fn signal(&self, count: usize) {
                let res = unsafe {
                    ReleaseSemaphore(self.handle.load(Ordering::Acquire), count as _, ptr::null_mut())
                };
                debug_assert_ne!(res, 0);
            }

            ///Performs deinitialization.
            ///
            ///Using `Sem` after `close` is undefined behaviour, unless `init` is called
            pub unsafe fn close(&self) {
                let handle = self.handle.swap(ptr::null_mut(), Ordering::AcqRel);
                if !handle.is_null() {
                    CloseHandle(handle);
                }
            }
        }

        impl Drop for Sem {
            fn drop(&mut self) {
                unsafe {
                    self.close();
                }
            }
        }

        unsafe impl Send for Sem {}
        unsafe impl Sync for Sem {}
    }

    #[cfg(windows)]
    pub use windows::*;
}

pub use os::Sem;

use crate::heap::thread::{Thread, SafeScope};

impl Sem {
    #[inline]
    pub fn wait_with_safepoint_check(&self) {
        // Prepare to block and allow safepoints while blocked
        let thread = Thread::current();

        let scope = SafeScope::new(thread);
        self.wait();
        drop(scope);
    }
}
