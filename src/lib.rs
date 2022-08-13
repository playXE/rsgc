#![feature(
    ptr_metadata,
    generic_const_exprs,
    const_type_id,
    const_type_name,
    const_ptr_offset_from,
    const_refs_to_cell,
    core_intrinsics,
    specialization
)]
#![allow(incomplete_features)]

pub mod array;
pub mod base;
pub mod memory;
pub mod weak_map;

pub use memory::{Managed, WeakField, WeakRef};

#[cfg(test)]
pub mod tests;