use std::ops::Deref;

use memoffset::offset_of;

use crate::{memory::{traits::{Trace, Finalize, ManagedObject, Allocation}, Heap}, Managed};

/// UTF-8 encoded immutable string allocated on GC heap.
#[repr(C)]
pub struct Str {
    len: usize,
    data: [u8;0]
}

impl Str {
    pub fn new(heap: &mut Heap, str: impl AsRef<str>) -> Managed<Self> {
        let str = str.as_ref();
        let len = str.len();
        let mut object = heap.varsize::<Self>(len);
        unsafe {
            let ptr = object.as_mut_ptr().cast::<u8>().add(Str::VARSIZE_OFFSETOF_VARPART);
            core::ptr::copy_nonoverlapping(str.as_ptr(), ptr, len);

            object.assume_init()
        }
    }
}

impl Deref for Str {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                self.data.as_ptr(),
                self.len
            ))
        }
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str {
        self
    }
}

impl AsRef<[u8]> for Str {
    fn as_ref(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.data.as_ptr(),
                self.len
            )
        }
    }
}

unsafe impl Allocation for Str {
    const VARSIZE: bool = true;
    const HAS_GCPTRS: bool = false;
    const VARSIZE_ITEM_SIZE: usize = 1;
    const VARSIZE_OFFSETOF_LENGTH: usize = offset_of!(Str, len);
    const VARSIZE_OFFSETOF_VARPART: usize = offset_of!(Str, data);
}

unsafe impl Trace for Str {}
unsafe impl Finalize for Str {}
impl ManagedObject for Str {}
