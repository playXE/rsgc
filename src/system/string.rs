use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};

use crate::thread::Thread;

use super::{
    object::{Allocation, Handle},
    traits::Object,
};

/// Simple zero-terminated UTF-8 encoded string allocated on heap.
#[repr(C)]
pub struct Str {
    length: u32,
    #[allow(dead_code)]
    pad: u32,
    data: [u8; 0],
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

unsafe impl Object for Str {}
unsafe impl Allocation for Str {
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
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                self.data.as_ptr(),
                self.length as _,
            ))
        }
    }
}

impl DerefMut for Str {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            std::str::from_utf8_unchecked_mut(std::slice::from_raw_parts_mut(
                self.data.as_mut_ptr(),
                self.length as _,
            ))
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

use super::arraylist::ArrayList;

use std::char::decode_utf16;
use std::ops::Range;
use std::str::*;

/// An equivalent of `std::string::String` but allocated on GC heap.
pub struct String {
    vec: ArrayList<u8>,
}

unsafe impl Object for String {
    fn trace(&self, visitor: &mut dyn crate::prelude::Visitor) {
        self.vec.trace(visitor)
    }
}

unsafe impl Allocation for String {}

impl String {
    pub fn new(thread: &mut Thread) -> Self {
        Self {
            vec: ArrayList::new(thread),
        }
    }

    pub fn with_capacity(thread: &mut Thread, capacity: usize) -> Self {
        Self {
            vec: ArrayList::with_capacity(thread, capacity),
        }
    }

    pub fn from_str(thread: &mut Thread, s: &str) -> Self {
        Self {
            vec: ArrayList::with(thread, s.len(), s.len(), |_, i| s.as_bytes()[i]),
        }
    }

    pub fn from_utf8(thread: &mut Thread, vec: &[u8]) -> Result<Self, Utf8Error> {
        match std::str::from_utf8(vec) {
            Ok(str) => Ok(Self::from_str(thread, str)),
            Err(e) => Err(e),
        }
    }

    pub fn from_utf8_lossy(thread: &mut Thread, vec: &[u8]) -> Self {
        Self::from_str(thread, &std::string::String::from_utf8_lossy(vec))
    }

    pub fn from_utf16(thread: &mut Thread, vec: &[u16]) -> Result<Self, &'static str> {
        let mut s = String::with_capacity(thread, vec.len());

        for c in decode_utf16(vec.iter().cloned()) {
            match c {
                Ok(c) => s.push(c),
                Err(_) => return Err("invalid utf16"),
            }
        }

        Ok(s)
    }

    pub fn push(&mut self, ch: char) {
        let thread = Thread::current();
        match ch.len_utf8() {
            1 => self.vec.push(thread, ch as u8),
            _ => {
                let mut buf = [0; 4];
                let s = ch.encode_utf8(&mut buf);

                for c in s.bytes() {
                    self.vec.push(thread, c);
                }
            }
        }
    }

    pub fn push_str(&mut self, s: &str) {
        let thread = Thread::current();
        for c in s.bytes() {
            self.vec.push(thread, c);
        }
    }

    pub fn len(&self) -> usize {
        self.vec.len()
    }

    pub fn capacity(&self) -> usize {
        self.vec.capacity()
    }

    pub fn len_utf8(&self) -> usize {
        self.chars().count()
    }

    pub fn len_utf16(&self) -> usize {
        self.encode_utf16().count()
    }

    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }

    pub fn clear(&mut self) {
        self.vec.clear();
    }

    pub fn truncate(&mut self, len: usize) {
        self.vec.truncate(len);
    }

    pub fn reserve(&mut self, additional: usize) {
        self.vec.reserve(Thread::current(), additional);
    }

    pub fn pop(&mut self) -> Option<char> {
        let ch = self.chars().rev().next()?;
        let newlen = self.len() - ch.len_utf8();
        unsafe {
            self.vec.set_len(newlen);
        }
        Some(ch)
    }

    pub fn remove(&mut self, idx: usize) -> char {
        let ch = match self[idx..].chars().next() {
            Some(ch) => ch,
            None => panic!("cannot remove a char from the end of a string"),
        };

        let next = idx + ch.len_utf8();
        let len = self.len();
        unsafe {
            std::ptr::copy(
                self.vec.as_ptr().add(next),
                self.vec.as_mut_ptr().add(idx),
                len - next,
            );
            self.vec.set_len(len - (next - idx));
        }
        ch
    }

    pub fn insert(&mut self, idx: usize, ch: char) {
        assert!(self.is_char_boundary(idx));
        let mut bits = [0; 4];
        let bits = ch.encode_utf8(&mut bits).as_bytes();

        unsafe {
            self.insert_bytes(idx, bits);
        }
    }

    unsafe fn insert_bytes(&mut self, idx: usize, bytes: &[u8]) {
        let len = self.len();
        let amt = bytes.len();
        self.vec.reserve(Thread::current(), amt);

        unsafe {
            std::ptr::copy(
                self.vec.as_ptr().add(idx),
                self.vec.as_mut_ptr().add(idx + amt),
                len - idx,
            );
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.vec.as_mut_ptr().add(idx), amt);
            self.vec.set_len(len + amt);
        }
    }

    pub fn insert_str(&mut self, idx: usize, string: &str) {
        assert!(self.is_char_boundary(idx));

        unsafe {
            self.insert_bytes(idx, string.as_bytes());
        }
    }

    pub unsafe fn as_mut_vec(&mut self) -> &mut ArrayList<u8> {
        &mut self.vec
    }

    pub fn split_off(&mut self, at: usize) -> Self {
        let other = self.vec.split_off(Thread::current(), at);
        Self { vec: other }
    }

    pub fn replace_range(&mut self, range: Range<usize>, replace_with: &str) {
        let start = range.start;
        let end = range.end;
        let amt = replace_with.len();
        let old_len = self.len();
        let new_len = old_len - (end - start) + amt;

        unsafe {
            self.vec.reserve(Thread::current(), amt);
            std::ptr::copy(
                self.vec.as_ptr().add(end),
                self.vec.as_mut_ptr().add(start + amt),
                old_len - end,
            );
            std::ptr::copy_nonoverlapping(
                replace_with.as_ptr(),
                self.vec.as_mut_ptr().add(start),
                amt,
            );
            self.vec.set_len(new_len);
        }
    }

    pub fn as_str(&self) -> &str {
        &self
    }
}

impl Deref for String {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        unsafe { from_utf8_unchecked(&self.vec) }
    }
}

impl DerefMut for String {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { from_utf8_unchecked_mut(&mut self.vec) }
    }
}

impl fmt::Debug for String {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl fmt::Display for String {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl PartialEq for String {
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(&**self, &**other)
    }
}

impl Eq for String {}

impl PartialOrd for String {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        PartialOrd::partial_cmp(&**self, &**other)
    }
}

impl Ord for String {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Ord::cmp(&**self, &**other)
    }
}

impl Hash for String {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Hash::hash(&**self, state)
    }
}

impl AsRef<str> for String {
    fn as_ref(&self) -> &str {
        &**self
    }
}

impl AsMut<str> for String {
    fn as_mut(&mut self) -> &mut str {
        &mut **self
    }
}

impl AsRef<[u8]> for String {
    fn as_ref(&self) -> &[u8] {
        &self.vec
    }
}

impl AsMut<[u8]> for String {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.vec
    }
}
