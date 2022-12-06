#![feature(const_type_id, const_type_name, panic_always_abort, core_intrinsics)]
#![allow(dead_code, unused_imports)]

pub const MEM_KIND_DYNAMIC: i32 = 0;
pub const MEM_KIND_RAW: i32 = 1;
pub const MEM_KIND_NOPTR: i32 = 2;
pub const MEM_KIND_FINALIZE: i32 = 3;
pub const MEM_ALIGN_DOUBLE: i32 = 128;
pub const MEM_ZERO: i32 = 256;

pub const fn mem_has_ptr(p: i32) -> bool {
    (!(p & 2)) != 0
}

pub mod gc;
//pub mod arena;
//pub mod weak_random;
pub mod env;
pub mod sync;
pub mod object;
pub mod bitfield;
pub mod heap;
pub mod traits;

pub struct FormattedSize {
    pub size: usize,
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

        if gsize < 1f64 {
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
    FormattedSize { size }
}