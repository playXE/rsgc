use memoffset::offset_of;

use super::object::*;
use super::traits::*;
use crate::heap::{thread::*, AllocError};
use std::{
    hash::Hash,
    mem::size_of,
    ops::{Deref, DerefMut},
};

/// GC allocated immutable array.
///
/// It derefs to `[T]` so you can use it just as a regular slice.
#[repr(C)]
pub struct Array<T: Object + Sized> {
    length: u32,
    init_length: u32,
    data: [T; 0],
}

impl<T: 'static + Object + Sized> Array<T> {
    /// Create a new array with the given length. Invokes `init` to produce the element at each index.
    pub fn new(
        th: &mut Thread,
        len: usize,
        mut init: impl FnMut(&mut Thread, usize) -> T,
    ) -> Handle<Self>
    where
        T: Allocation,
    {
        let mut result = th.allocate_varsize::<Self>(len);
        let arr = result.as_mut().as_mut_ptr();
        for i in 0..len {
            unsafe {
                let data = (*arr).data.as_mut_ptr().add(i);

                let res = init(th, i);
                th.write_barrier(result);
                data.write(res);
                (*arr).init_length += 1;
            }
        }
        unsafe { result.assume_init() }
    }
}

impl<T: Object> Deref for Array<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        unsafe { std::slice::from_raw_parts(self.data.as_ptr(), self.length as _) }
    }
}

impl<T: Object> DerefMut for Array<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { std::slice::from_raw_parts_mut(self.data.as_mut_ptr(), self.length as _) }
    }
}

impl<T: Object> Array<T> {
    pub fn len(&self) -> usize {
        self.length as _
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index < self.len() {
            Some(&self[index])
        } else {
            None
        }
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index < self.len() {
            Some(&mut self[index])
        } else {
            None
        }
    }

    pub fn set(&mut self, index: usize, val: T) -> Option<T> {
        if index < self.len() {
            let old = std::mem::replace(&mut self[index], val);
            Some(old)
        } else {
            None
        }
    }
}

unsafe impl<T: Object + ?Sized> Object for Handle<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        unsafe {
            visitor.visit(self.ptr.as_ptr());
        }
    }
}

unsafe impl<T: Object> Object for Array<T> {
    fn finalize(&mut self) {
        unsafe {
            core::ptr::drop_in_place(std::slice::from_raw_parts_mut(
                self.data.as_mut_ptr(),
                self.init_length as _,
            ));
        }
    }

    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn Visitor) {
        unsafe {
            if from >= self.init_length as usize {
                return;
            }
            let slice = std::slice::from_raw_parts(self.data.as_ptr().add(from), to - from);
            let mut actual_index = from;
            for val in slice.iter() {
                if actual_index < self.init_length as usize {
                    val.trace(visitor);
                } else {
                    return;
                }
                actual_index += 1;
            }
        }
    }

    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }

    fn process_weak(&mut self, processor: &mut dyn WeakProcessor) {
        unsafe {
            let slice =
                std::slice::from_raw_parts_mut(self.data.as_mut_ptr(), self.init_length as _);

            for val in slice {
                val.process_weak(processor);
            }
        }
    }
}

unsafe impl<T: Object + Sized + Allocation> Allocation for Array<T> {
    const FINALIZE: bool = std::mem::needs_drop::<T>();
    const DESTRUCTIBLE: bool = true;
    const SIZE: usize = size_of::<Self>();
    const NO_HEAP_PTRS: bool = true;
    const VARSIZE_NO_HEAP_PTRS: bool = T::NO_HEAP_PTRS || (T::VARSIZE && T::VARSIZE_NO_HEAP_PTRS);
    const VARSIZE: bool = true;
    const VARSIZE_ITEM_SIZE: usize = size_of::<T>();
    const VARSIZE_OFFSETOF_LENGTH: usize = offset_of!(Self, length);
    const VARSIZE_OFFSETOF_CAPACITY: usize = 0;
    const VARSIZE_OFFSETOF_VARPART: usize = size_of::<usize>();
}
use std::fmt;

impl<T: fmt::Debug + Object> fmt::Debug for Array<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Object + PartialEq<U>, U: Object + PartialEq<T>> PartialEq<Array<U>> for Array<T> {
    fn eq(&self, other: &Array<U>) -> bool {
        self.as_ref().eq(other.as_ref())
    }
}

impl<T: Object + Eq> Eq for Array<T> {}

impl<T: Object + Hash> Hash for Array<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state);
    }
}

impl<T: Object> AsRef<[T]> for Array<T> {
    fn as_ref(&self) -> &[T] {
        self
    }
}

impl<T: Object> AsMut<[T]> for Array<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Object + PartialOrd> PartialOrd for Array<T> {
    fn partial_cmp(&self, other: &Array<T>) -> Option<std::cmp::Ordering> {
        self.as_ref().partial_cmp(other.as_ref())
    }
}

impl<T: Object + Ord> Ord for Array<T> {
    fn cmp(&self, other: &Array<T>) -> std::cmp::Ordering {
        self.as_ref().cmp(other.as_ref())
    }
}
