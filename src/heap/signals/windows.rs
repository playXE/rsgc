use winapi::um::minwinbase::EXCEPTION_ACCESS_VIOLATION;
use winapi::um::winnt::LONG;
use winapi::um::winnt::PEXCEPTION_POINTERS;
use winapi::vc::excpt::EXCEPTION_CONTINUE_EXECUTION;
use winapi::vc::excpt::EXCEPTION_CONTINUE_SEARCH;

use crate::heap::heap::heap;
use crate::heap::safepoint;
use crate::prelude::Thread;
pub fn install_signal_handlers() {}

pub unsafe extern "C" fn exception_handler(exception_info: PEXCEPTION_POINTERS) -> LONG {
    if (*(*exception_info).ExceptionRecord).ExceptionFlags != 0 {
        return EXCEPTION_CONTINUE_SEARCH;
    }

    match (*(*exception_info).ExceptionRecord).ExceptionCode {
        EXCEPTION_ACCESS_VIOLATION => {
            if safepoint::addr_in_safepoint(
                (*(*exception_info).ExceptionRecord).ExceptionInformation[1] as usize,
            ) {
                let thread = Thread::current();

                thread.platform_registers = (*exception_info).ContextRecord;
                let sp = (*thread.platform_registers).Rsp;
                thread.enter_safepoint(sp as _);
                thread.platform_registers = std::ptr::null_mut();
                return EXCEPTION_CONTINUE_EXECUTION;
            } else {
                let heap = heap();

                if heap.is_in((*(*exception_info).ExceptionRecord).ExceptionInformation[1] as _) {
                    // we got here because we tried to access heap memory that is not mapped
                    // this can happen if we try to access memory that is not allocated yet
                    // or if we try to access memory that was already freed
                    // we can't do anything about it, so we just die

                    let backtrace = std::backtrace::Backtrace::force_capture();

                    eprintln!(
                        "FATAL: Heap Out of Bounds Access of 0x{:x}",
                        (*(*exception_info).ExceptionRecord).ExceptionInformation[1]
                    );
                    eprintln!("Probably tried to access uncommited region or it is a bug in GC: report it to https://github.com/playxe/rsgc");
                    eprintln!("{}", backtrace);

                    EXCEPTION_CONTINUE_SEARCH
                } else {
                    EXCEPTION_CONTINUE_SEARCH
                }
            }
        }

        _ => EXCEPTION_CONTINUE_SEARCH,
    }
}
