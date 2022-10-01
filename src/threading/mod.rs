use std::ptr::NonNull;

use crate::memory::{page::LinearAllocationBuffer, HeapRef};

pub struct Mutator {
    pub(crate) refs: usize,
    pub(crate) tlab: LinearAllocationBuffer,
    pub(crate) heap: HeapRef
}

pub struct MutatorRef(NonNull<Mutator>);