use std::ptr::null_mut;

use libc::*;

use crate::heap::{stack::approximate_stack_pointer, safepoint, thread::thread};

unsafe extern "C" fn sigdie_handler(sig: i32, _info: *mut siginfo_t, _context: *mut c_void) {
    let mut sset = 0;
    sigfillset(&mut sset);
    sigprocmask(SIG_UNBLOCK, &mut sset, null_mut());
    signal(sig, SIG_DFL);

    if sig != SIGSEGV && sig != SIGBUS && sig != SIGILL {
        raise(sig);
    }

    // fall-through return to re-execute faulting statement (but without the error handler)
}

unsafe extern "C" fn segv_handler(sig: i32, info: *mut siginfo_t, context: *mut ucontext_t) {
    // polling page was protected and some thread tried to read from it
    // and we got here. Polling page gets protected only when safepoint is requested. 
    if safepoint::addr_in_safepoint((*info).si_addr() as usize) {
        log::trace!(target: "gc-safepoint", "{:?} reached safepoint", std::thread::current().id());
        // blocks until safepoint is disabled.
        thread().enter_safepoint(get_sp_from_ucontext(context).cast());
        log::trace!(target: "gc-safepoint", "{:?} exit safepoint", std::thread::current().id());
        return;
    }

    sigdie_handler(sig, info, context as _);
}

unsafe fn get_sp_from_ucontext(uc: *mut ucontext_t) -> *mut () {
    cfg_if::cfg_if! {
        if #[cfg(any(target_os="macos", target_os="ios", target_os="tvos", target_os="watchos"))]
        {
            #[cfg(any(target_arch="aarch64", target_arch="arm"))]
            {
                
                (*(*uc).uc_mcontext).__ss.__sp as _
            }
            #[cfg(target_arch="x86_64")]
            {
                (*(*uc).uc_mcontext).__ss.__rsp
            }
            #[cfg(target_arch="x86")]
            {
                (*(*uc).uc_mcontext).__ss.__esp
            }
        } else {
            approximate_stack_pointer() as _
        }
    }
}

pub fn install_signal_handlers() {
    unsafe {
        let mut act: sigaction = std::mem::MaybeUninit::<sigaction>::zeroed().assume_init();
        
        sigemptyset(&mut act.sa_mask);
        act.sa_sigaction = segv_handler as _;
        act.sa_flags = SA_SIGINFO;

        if sigaction(SIGSEGV, &act, null_mut()) < 0 {
            panic!("failed to set SIGSEGV handler for safepoints");
        }

        // on AArch64 SIGBUS is thrown when accessing undefined memory.
        if sigaction(SIGBUS, &act, null_mut()) < 0 {
            panic!("failed to set SIGBUS handler for safepoints");
        }
    }
}