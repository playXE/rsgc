#![feature(
    const_type_id,
    const_refs_to_cell,
    const_type_name,
    panic_always_abort,
    ptr_metadata,
    core_intrinsics,
    specialization,
    thread_local,
    ptr_sub_ptr,
    portable_simd,
    arbitrary_self_types
)]
#![allow(dead_code, unused_imports, incomplete_features)]

use std::{cell::UnsafeCell, sync::atomic::AtomicPtr};

pub const MEM_KIND_DYNAMIC: i32 = 0;
pub const MEM_KIND_RAW: i32 = 1;
pub const MEM_KIND_NOPTR: i32 = 2;
pub const MEM_KIND_FINALIZE: i32 = 3;
pub const MEM_ALIGN_DOUBLE: i32 = 128;
pub const MEM_ZERO: i32 = 256;

pub const fn mem_has_ptr(p: i32) -> bool {
    (!(p & 2)) != 0
}

pub mod bitfield;
pub mod env;
pub mod heap;
pub mod sync;
pub mod system;
pub mod utils;

pub struct FormattedSize {
    pub size: f64,
}

impl std::fmt::Display for FormattedSize {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let ksize = (self.size as f64) / 1024f64;

        if ksize < 1f64 {
            return write!(f, "{}B", self.size);
        }

        let msize = ksize / 1024f64;

        if msize < 1f64 {
            return write!(f, "{:.1}K", ksize);
        }

        let gsize = msize / 1024f64;

        if gsize < 8f64 {
            write!(f, "{:.1}M", msize)
        } else {
            write!(f, "{:.1}G", gsize)
        }
    }
}

impl std::fmt::Debug for FormattedSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

pub fn formatted_size(size: usize) -> FormattedSize {
    FormattedSize { size: size as f64 }
}

pub fn formatted_sizef(size: f64) -> FormattedSize {
    FormattedSize { size: size as f64 }
}

static mut SINK: usize = 0;

pub fn force_on_stack<T>(val: *const T) {
    unsafe {
        core::ptr::write_volatile(&mut SINK, val as usize);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
}

#[macro_export]
macro_rules! offsetof {
    ($obj: ty, $($field: ident).+) => {{
        #[allow(unused_unsafe)]
        unsafe {
            let addr = 0x4000 as *const $obj;
            &(*addr).$($field).* as *const _ as usize - 0x4000
        }
    }
    };
}

pub use heap::thread;
use system::object::Allocation;

/// Returns true if write barrier is required when writing to a field of type `T`.
///
/// Write barrier is required only if `T` contains heap pointers.
pub const fn needs_write_barrier<T: Allocation>() -> bool {
    !T::NO_HEAP_PTRS || (T::VARSIZE && !T::VARSIZE_NO_HEAP_PTRS)
}

pub mod prelude {
    pub use super::heap;
    pub use super::system;
    pub use heap::thread::*;
    pub use heap::region::HeapArguments;
    pub use system::{object::*, traits::*};
}
