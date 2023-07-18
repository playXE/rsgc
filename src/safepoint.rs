use std::{
    mem::{size_of, MaybeUninit},
    sync::atomic::{AtomicBool, AtomicPtr, Ordering, fence},
};

use crate::{
    stack::approximate_stack_pointer,
    thread::Thread,
    utils::mcontext::{get_sp_from_ucontext, registers_from_ucontext}, ThreadState, heap::Heap,
};

pub unsafe fn safepoint_init(ptr: &mut *mut u8) {
    let addr: *mut u8;
    #[cfg(windows)]
    {
        use winapi::um::{memoryapi::*, winnt::*};

        addr = VirtualAlloc(
            core::ptr::null_mut(),
            size_of::<usize>(),
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READONLY,
        ) as _;
        if (addr as usize) == 0 {
            panic!("VirtualAlloc failed");
        }
    }

    #[cfg(not(windows))]
    {
        addr = libc::mmap(
            core::ptr::null_mut(),
            size_of::<usize>(),
            libc::PROT_READ,
            libc::MAP_NORESERVE | libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        ) as _;
        if addr == libc::MAP_FAILED.cast::<u8>() {
            panic!("mmap failed");
        }
    }

    *ptr = addr;
}

pub unsafe fn safepoint_arm(ptr: *mut u8) {
    #[cfg(windows)]
    {
        use winapi::um::{memoryapi::*, winnt::*};

        let mut old_protect = 0;
        if VirtualProtect(
            ptr as _,
            size_of::<usize>(),
            PAGE_NOACCESS,
            &mut old_protect,
        ) == 0
        {
            panic!("VirtualProtect failed");
        }
    }

    #[cfg(not(windows))]
    {
       
        if libc::mprotect(ptr as _, size_of::<usize>(), libc::PROT_NONE) != 0 {
            panic!("mprotect failed");
        }
    }
}

pub unsafe fn safepoint_disarm(ptr: *mut u8) {
    #[cfg(windows)]
    {
        use winapi::um::{memoryapi::*, winnt::*};

        let mut old_protect = 0;
        if VirtualProtect(
            ptr as _,
            size_of::<usize>(),
            PAGE_READONLY,
            &mut old_protect,
        ) == 0
        {
            panic!("VirtualProtect failed");
        }
    }

    #[cfg(not(windows))]
    {
        if libc::mprotect(ptr as _, size_of::<usize>(), libc::PROT_READ) != 0 {
            panic!("mprotect failed");
        }
    }
}

static COLLECTING: AtomicBool = AtomicBool::new(false);
static SYNCHRONIZER_LOCK: parking_lot::Mutex<()> = parking_lot::const_mutex(());
static SYNCHRONIZER_COND: parking_lot::Condvar = parking_lot::Condvar::new();
pub(crate) static SAFEPOINT_PAGE: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());

cfg_if::cfg_if! {
    if #[cfg(not(windows))]
    {
        use libc::ucontext_t;
        unsafe extern "C" fn sigdie_handler(sig: i32, _info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
            let mut sset = MaybeUninit::<libc::sigset_t>::zeroed().assume_init();
            libc::sigfillset(&mut sset);
            libc::sigprocmask(libc::SIG_UNBLOCK, &mut sset, core::ptr::null_mut());
            libc::signal(sig, libc::SIG_DFL);

            if sig != libc::SIGSEGV && sig != libc::SIGBUS && sig != libc::SIGILL {
                libc::raise(sig);
            }

            // fall-through return to re-execute faulting statement (but without the error handler)
        }

        unsafe extern "C" fn segv_handler(signal: i32, siginfo: *mut libc::siginfo_t, context: *mut ucontext_t) {
            unsafe {
                if (*siginfo).si_addr().cast::<u8>() == SAFEPOINT_PAGE.load(Ordering::Relaxed) {
                    let thread = Thread::current();
                    log::debug!(target: "gc", "{:?}({}) reached safepoint", std::thread::current().id(), std::thread::current().name().unwrap_or("<unnamed>"));
                    thread.stack_top.store(get_sp_from_ucontext(context).cast(), Ordering::Relaxed);
                    thread.execution_context.as_mut_ptr().write(registers_from_ucontext(context).read());
                    thread.set_gc_and_wait();
                } else {
                    eprintln!("Unexpected signal {} when accessing memory at address: {:p}", signal, (*siginfo).si_addr());
                    eprintln!("{}", std::backtrace::Backtrace::force_capture());
                    sigdie_handler(signal, siginfo, context as _)
                }
            }
        }
    }
}

pub(crate) fn install_signal_handlers() {
    use libc::*;
    unsafe {
        let mut act: sigaction = std::mem::MaybeUninit::<sigaction>::zeroed().assume_init();

        sigemptyset(&mut act.sa_mask);
        act.sa_sigaction = segv_handler as _;
        act.sa_flags = SA_SIGINFO;

        if sigaction(SIGSEGV, &act, std::ptr::null_mut()) < 0 {
            panic!("failed to set SIGSEGV handler for safepoints");
        }

        // on AArch64 SIGBUS is thrown when accessing undefined memory.
        if sigaction(SIGBUS, &act, std::ptr::null_mut()) < 0 {
            panic!("failed to set SIGBUS handler for safepoints");
        }
    }
}

pub(crate) fn wait() {
    while COLLECTING.load(Ordering::Relaxed) || COLLECTING.load(Ordering::Acquire) {
        let mut guard = SYNCHRONIZER_LOCK.lock();

        if COLLECTING.load(Ordering::Relaxed) {
            SYNCHRONIZER_COND.wait(&mut guard);
        }

        drop(guard);
    }
}

pub(crate) unsafe fn enter(thread: &mut Thread) -> bool {
    let tptr = thread as *mut Thread;
    let actual_enter = || {
        let guard = SYNCHRONIZER_LOCK.lock();
        match COLLECTING.compare_exchange_weak(false, true, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => {
                safepoint_arm(SAFEPOINT_PAGE.load(Ordering::Relaxed));
                drop(guard);
                let heap = Heap::get();
                loop {
                    fence(Ordering::SeqCst);
                    
                    let mut node = heap.threads;
                    let mut active = 0;
                    while !node.is_null() {
                        if !(*node).thread.is_waiting.load(Ordering::Acquire) && (*node).thread as *mut Thread != tptr {
                            active += 1;
                        }

                        node = (*node).next;
                    }
                    
                    if active == 0 {
                        break;
                    } else {
                        std::thread::yield_now();
                    }
                }
                true
            }
            Err(_) => {
                // In case multiple threads enter the GC at the same time, only allow
                // one of them to actually run the collection.
                drop(guard);
                wait();
                false
            }
        }
    };
    thread.state = ThreadState::Unmanaged;
    thread
        .stack_top
        .store(approximate_stack_pointer() as _, Ordering::Relaxed);
    #[cfg(not(windows))]
    let res = {
        extern "C" {
            #[allow(improper_ctypes)]
            fn getcontext(ctx: *mut libc::ucontext_t) -> i32;
        }
        let mut ctx = MaybeUninit::<libc::ucontext_t>::zeroed().assume_init();
        getcontext(&mut ctx as *mut _);

        thread.execution_context.as_mut_ptr().write(registers_from_ucontext(&mut ctx).read());
        let res = actual_enter();

        res
    };

    thread.state = ThreadState::Managed;

    res
}

pub(crate) fn leave() {
    let guard = SYNCHRONIZER_LOCK.lock();

    unsafe {
        safepoint_disarm(SAFEPOINT_PAGE.load(Ordering::Relaxed));
    }

    COLLECTING.store(false, Ordering::Release);
    SYNCHRONIZER_COND.notify_all();
    drop(guard);
}
