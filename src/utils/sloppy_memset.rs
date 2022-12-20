// Thanks to zuur#9735 (thomcc on GH) for code.

//! "Sloppy" pattern memsets.
//!
//! - A "pattern" memset is a memset where rather than taking a single byte
//!   to fill the memory with, it takes a `u16`, `u32`, `u64`, ... instead.
//!   This is useful, for example, if you need the bytes to alternate value
//!   (or, well, match some other pattern, ...)
//!
//!    The closest comparison in the Rust stdlib is probably [`slice::fill`],
//!    but these are unsafe, operating on raw pointers, and more importantly,
//!    these are also "sloppy".
//!
//! - "Sloppy" in this case refers to the fact that these may write up to
//!   16 bytes past the end of the `ptr`/`len` pair they are provided.
//!   (Obviously this is unsafe, and must be done carefully). This is
//!   sometimes desirable for performance (upwards of 10% in the decoder)
//!
//! Anyway, watch out for accidentally narrowed provenance, and be sure
//! you allocate padding/slop for this. For example:
//!
//! ```ignore
//! const SYMBOLS: usize = 32;
//!
//! struct HistFreqs {
//!     // `+ 8` for use with sloppy memset.
//!     counts: [u16; SYMBOLS + 8],
//! }
//! // Then
//! unsafe { sloppy_memset_pat16(some_hist_freqs.counts.as_mut_ptr()) }
//! ```

use super::is_aligned;
use core::{mem::*, simd::*};

/// Similar to `core::ptr::write_bytes(ptr, v, len)` but may
/// write up to 16 bytes past the end (in exchange for perf).
///
/// Safety:
/// - `ptr` must be valid for a write of `len + 16` (which
///   must not overflow) bytes.
///
/// Important note: this is "sloppy", and the values outside of the
/// `0..len` range (that is, the final 16 items in the "trailing
/// slop") may or may not be written to. You
/// should not use this function if that could be a problem.
#[inline(always)]
pub unsafe fn sloppy_memset(ptr: *mut u8, value: u8, len: usize) {
    simd_sloppy_memset(ptr, u8x16::splat(value), len);
}

/// Fill `ptr..ptr.add(count)` with `val`. Note that `count`
/// is in units of `u16`, not `u8`, possibly writing no more
/// than 16 bytes past the end (to go fast).
///
/// Safety:
/// - `ptr` must be aligned to `align_of::<u16>()`.
/// - `ptr` must be valid for a write of `count * size_of::<u16>() + 16`
///   bytes (where that expression must not overflow, of course).
///
/// Important note: this is "sloppy", and the values outside of the
/// `0..count` range (that is, the final `(16 / size_of::<u16>()) == 2`
/// items in the "trailing slop") may or may not be written to. You
/// should not use this function if that could be a problem.
#[inline(always)]
pub unsafe fn sloppy_memset_pat16(ptr: *mut u16, value: u16, count: usize) {
    // `u16x8::splat` codegen sucks on x86 (rust-lang/portable-simd#283)
    // so we use core::arch (worth doing since the `_pat16` version
    // basically is the is reason these functions exist)
    #[cfg(all(
        target_feature = "sse2",
        any(target_arch = "x86_64", target_arch = "x86"),
    ))]
    let splat: u8x16 = transmute(core::arch::x86_64::_mm_set1_epi16(value as i16));

    #[cfg(not(all(
        target_feature = "sse2",
        any(target_arch = "x86_64", target_arch = "x86"),
    )))]
    let splat: u8x16 = transmute(u16x8::splat(value));

    simd_sloppy_memset(ptr, splat, count);
}

/// Fill `ptr..ptr.add(count)` with `val`. Note that `count`
/// is in units of `u32`, not `u8`, possibly writing no more
/// than 16 bytes past the end (to go fast).
///
/// Safety:
/// - `ptr` must be aligned to `align_of::<u32>()`.
/// - `ptr` must be valid for a write of `count * size_of::<u32>() + 16`
///   bytes (where that expression must not overflow, of course).
///
/// Important note: this is "sloppy", and the values outside of the
/// `0..count` range (that is, the final `(16 / size_of::<u32>()) == 4`
/// items in the "trailing slop") may or may not be written to. You
/// should not use this function if that could be a problem.
#[inline(always)]
pub unsafe fn sloppy_memset_pat32(ptr: *mut u32, value: u32, count: usize) {
    let splat: u8x16 = transmute(u32x4::splat(value));
    simd_sloppy_memset(ptr, splat, count);
}

/// Fill `ptr..ptr.add(count)` with `val`. Note that `count`
/// is in units of `u64`, not `u8`, possibly writing no more
/// than 16 bytes past the end (to go fast).
///
/// Safety:
/// - `ptr` must be aligned to `align_of::<u64>()`.
/// - `ptr` must be valid for a write of `count * size_of::<u64>() + 16`
///   bytes (where that expression must not overflow, of course).
///
/// Important note: this is "sloppy", and the values outside of the
/// `0..count` range (that is, the final `(16 / size_of::<u64>()) == 2`
/// items in the "trailing slop") may or may not be written to. You
/// should not use this function if that could be a problem.
#[inline(always)]
pub unsafe fn sloppy_memset_pat64(ptr: *mut u64, value: u64, count: usize) {
    let splat: u8x16 = transmute(u64x2::splat(value));
    simd_sloppy_memset(ptr, splat, count);
}

#[inline(always)]
unsafe fn simd_sloppy_memset<T>(ptr: *mut T, splat: u8x16, count: usize) {
    // paranoia checks
    assert!(
        size_of::<T>() == align_of::<T>()
            && size_of::<T>() <= 16
            && size_of::<T>().is_power_of_two()
    );
    debug_assert!(is_aligned(ptr as usize, align_of::<T>(), 0));

    // Write the unaligned head.
    ptr.cast::<u8x16>().write_unaligned(splat);

    if count <= (16 / size_of::<T>()) {
        // that covered everything!
        return;
    }
    // Write the body
    let end = ptr.add(count).cast::<u8>();
    let mut p: *mut u8 = {
        // Advance past the part we wrote
        let tmp_p = ptr.cast::<u8>().add(16);
        // Align down to 16. We'll write some
        // bytes multiple times, but it's fine
        tmp_p.sub(tmp_p as usize & 0xf)
    };
    // paranoia check
    debug_assert!(
        is_aligned(p as usize, align_of::<u8x16>(), 0) && (p.cast::<u8>() > ptr.cast::<u8>())
    );
    loop {
        // Miri does not believe it's possible to manually
        // align pointers :/
        if cfg!(miri) {
            p.cast::<u8x16>().write_unaligned(splat);
        } else {
            p.cast::<u8x16>().write(splat);
        }
        p = p.add(16);
        if p >= end {
            break;
        }
    }
}

// This is where our sloppiness threshold comes from.
const _: () = assert!(align_of::<u8x16>() == 16);
const _: () = assert!(size_of::<u8x16>() == 16);

