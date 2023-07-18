cfg_if::cfg_if! {
    if #[cfg(any(target_os = "macos", target_os = "ios", target_os = "watchos", target_os = "tvos"))] {
        #[cfg(target_arch="aarch64")]
        pub type PlatformRegisters = libc::__darwin_arm_thread_state64;

        #[cfg(target_arch="x86_64")]
        pub type PlatformRegisters = libc::__darwin_x86_thread_state64_t;

        #[cfg(target_arch="x86")]
        pub type PlatformRegisters = libc::__darwin_x86_thread_state32_t;

        #[cfg(target_arch="arm")]
        pub type PlatformRegisters = libc::__darwin_arm_thread_state;

        pub fn registers_from_ucontext(ucontext: *mut libc::ucontext_t) -> *mut PlatformRegisters {
            unsafe {
                &mut (*(*ucontext).uc_mcontext).__ss as *mut PlatformRegisters
            }
        }

    } else if #[cfg(windows)] {
        pub type PlatformRegisters = winapi::um::winnt::CONTEXT;

        pub fn registers_from_ucontext(ucontext: *mut winapi::um::winnt::CONTEXT) -> *mut PlatformRegisters {
            ucontext
        }
    } else {
        pub struct PlatformRegisters {
            pub machine_context: libc::mcontext_t
        }

        pub fn registers_from_ucontext(ucontext: *mut libc::ucontext_t) -> *mut PlatformRegisters {
            unsafe {
                cfg_if::cfg_if! {
                    if #[cfg(target_os="openbsd")] {
                        ucontext.cast()
                    } else {
                        &mut (*ucontext).uc_mcontext as *mut libc::mcontext_t as *mut PlatformRegisters
                    }
                }

            }
        }
    }
}

#[cfg(not(windows))]

pub unsafe fn get_sp_from_ucontext(uc: *mut libc::ucontext_t) -> *mut () {
    use crate::stack::approximate_stack_pointer;

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
            let _ = uc;
            // Darwin has the sanest ucontext_t impl, others don't so we use our own code
            approximate_stack_pointer() as _
        }
    }
}