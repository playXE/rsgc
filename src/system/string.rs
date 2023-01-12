use std::{ops::{Deref, DerefMut}, hash::Hash};

use crate::thread::Thread;

use super::{traits::Object, object::{Allocation, Handle}};

/// Simple zero-terminated UTF-8 encoded string allocated on heap.
#[repr(C)]
pub struct Str {
    length: u32,
    #[allow(dead_code)]
    pad: u32,
    data: [u8; 0]
}

impl Str {
    pub fn len(&self) -> usize {
        self.length as _
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn new(thread: &mut Thread, init: impl AsRef<str>) -> Handle<Str> {
        unsafe {
            let init = init.as_ref();
            let mut result = thread.allocate_varsize::<Str>(init.len() + 1);
            let result_ref = result.assume_init_mut();
            result_ref.length = init.len() as u32 + 1;
            std::slice::from_raw_parts_mut(result_ref.data.as_mut_ptr(), init.len())
                .copy_from_slice(init.as_bytes());
            result_ref.data.as_mut_ptr().add(init.len()).write(0);
            result.assume_init()
        }
    }
}

impl Object for Str {}
impl Allocation for Str {
    const NO_HEAP_PTRS: bool = true;
    const VARSIZE: bool = true;
    const VARSIZE_NO_HEAP_PTRS: bool = true;
    const VARSIZE_ITEM_SIZE: usize = 1;
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    const VARSIZE_OFFSETOF_VARPART: usize = memoffset::offset_of!(Str, data);
}

impl Deref for Str {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        unsafe {
            std::str::from_utf8_unchecked(
                std::slice::from_raw_parts(self.data.as_ptr(), self.length as _)
            )
        }
    }
}

impl DerefMut for Str {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            std::str::from_utf8_unchecked_mut(
                std::slice::from_raw_parts_mut(self.data.as_mut_ptr(), self.length as _)
            )
        }
    }
}

impl AsRef<str> for Str {
    fn as_ref(&self) -> &str {
        self
    }
}

impl AsMut<str> for Str {
    fn as_mut(&mut self) -> &mut str {
        self
    }
}
use std::fmt;
impl fmt::Display for Str {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl fmt::Debug for Str {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl PartialEq for Str {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}

impl Eq for Str {}

impl PartialEq<str> for Str {
    fn eq(&self, other: &str) -> bool {
        self.as_ref() == other
    }
}

impl Hash for Str {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state)
    }
}
