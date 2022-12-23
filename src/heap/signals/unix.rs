use std::ptr::null_mut;

use libc::*;

use crate::{
    heap::{heap::heap, safepoint, stack::approximate_stack_pointer, thread::Thread},
    utils::machine_context::{registers_from_ucontext, PlatformRegisters},
};

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
        let thread = Thread::current();
        thread.platform_registers = registers_from_ucontext(context);

        log::trace!(target: "gc-safepoint", "{:?} reached safepoint", std::thread::current().id());
        // basically spin-loop that waits for safepoint to be disabled
        thread.enter_safepoint(get_sp_from_ucontext(context).cast());
        thread.platform_registers = null_mut();
        log::trace!(target: "gc-safepoint", "{:?} exit safepoint", std::thread::current().id());
        return;
    } else {
        let heap = heap();

        if heap.is_in((*info).si_addr().cast()) {
            // we got here because we tried to access heap memory that is not mapped
            // this can happen if we try to access memory that is not allocated yet
            // or if we try to access memory that was already freed
            // we can't do anything about it, so we just die

            let backtrace = std::backtrace::Backtrace::force_capture();

            eprintln!(
                "FATAL: Heap Out of Bounds Access of {:p}",
                (*info).si_addr()
            );
            eprintln!("Probably tried to access uncommited region or it is a bug in GC: report it to https://github.com/playxe/rsgc");
            eprintln!("{}", backtrace);

            return sigdie_handler(sig, info, context as _);
        }
    }

    println!(
        "FATAL: Unhandled signal {}: {:p}, backtrace: \n{}",
        sig,
        (*info).si_addr(),
        std::backtrace::Backtrace::force_capture()
    );

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
                (*(*uc).uc_mcontext).__ss.__rsp as _
            }
            #[cfg(target_arch="x86")]
            {
                (*(*uc).uc_mcontext).__ss.__esp as _
            }
        } else {
            // Darwin has the sanest ucontext_t impl, others don't so we use our own code
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
