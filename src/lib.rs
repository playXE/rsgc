#![feature(
    ptr_metadata,
    link_llvm_intrinsics,
    generic_const_exprs,
    const_type_id,
    const_type_name,
    const_ptr_offset_from,
    hash_raw_entry,
    const_refs_to_cell,
    core_intrinsics
)]
#![allow(incomplete_features)]

pub mod array;
pub mod base;
pub mod memory;
pub mod weak_map;

pub use memory::{Managed, WeakField, WeakRef};
