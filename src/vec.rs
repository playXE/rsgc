use std::{
    mem::{size_of, MaybeUninit},
    ops::{Deref, DerefMut, Index, IndexMut}, hash::Hash,
};

use memoffset::offset_of;

use crate::{
    base::{constants::OBJECT_ALIGNMENT, utils::round_up},
    memory::{
        traits::{Allocation, Finalize, ManagedObject, Trace},
        Heap,
    },
    Managed,
};

#[repr(C)]
struct Raw<T> {
    len: usize,
    cap: usize,
    data: [MaybeUninit<T>; 0],
}

unsafe impl<T: Allocation> Allocation for Raw<T> {
    const FINALIZE: bool = T::FINALIZE;
    const LIGHT_FINALIZER: bool = {
        if T::FINALIZE {
            assert!(
                T::LIGHT_FINALIZER,
                "type must be light finalizable in order to be stored in ManagedVec",
            );
            true
        } else {
            false
        }
    };
    const VARSIZE: bool = true;
    const VARSIZE_ITEM_SIZE: usize = size_of::<T>();
    const VARSIZE_OFFSETOF_LENGTH: usize = offset_of!(Raw<T>, cap);
    const VARSIZE_OFFSETOF_VARPART: usize = offset_of!(Raw<T>, data);
}

unsafe impl<T: Trace> Trace for Raw<T> {
    fn trace(&self, visitor: &mut dyn crate::memory::visitor::Visitor) {
        let mut cursor = self.data.as_ptr();
        for _ in 0..self.len {
            unsafe {
                let item = &*cursor.cast::<T>();
                item.trace(visitor);
                cursor = cursor.add(1);
            }
        }
    }
}

unsafe impl<T: Trace + Finalize> Finalize for Raw<T> {
    fn finalize(&mut self) {
        let ptr = self.data.as_mut_ptr();
        for i in 0..self.len {
            unsafe {
                let item = ptr.add(i);
                (*item).finalize();
            }
        }
    }
}

impl<T: Trace + Finalize> ManagedObject for Raw<T> {}

impl<T: Trace + Finalize> Deref for Raw<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        unsafe { std::slice::from_raw_parts(self.data.as_ptr().cast(), self.len) }
    }
}

impl<T: Trace + Finalize> DerefMut for Raw<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { std::slice::from_raw_parts_mut(self.data.as_mut_ptr().cast(), self.len) }
    }
}


/// Managed version of [`Vec`]. Methods that reallocate simply get another
/// parameter of type `&mut Heap` to allocate from.
pub struct ManagedVec<T: Trace + Finalize> {
    raw: Managed<Raw<T>>,
}

impl<T: Trace + Finalize + Allocation + 'static> ManagedVec<T> {
    pub fn len(&self) -> usize {
        self.raw.len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.raw.cap
    }

    fn data(&self) -> *mut T {
        self.raw.data.as_ptr() as _
    }

    fn grow(&mut self, heap: &mut Heap, capacity: usize) {
        debug_assert!(capacity >= self.len());

        let old_capacity = self.capacity();
        let new_capacity = capacity;

        if old_capacity == new_capacity {
            return;
        }

        let len = self.len();

        unsafe {
            let mut new_buf = heap.varsize::<Raw<T>>(new_capacity).assume_init();
            new_buf.len = len;
            core::ptr::copy_nonoverlapping(self.raw.data.as_ptr(), new_buf.data.as_mut_ptr(), len);

            self.raw = new_buf;
        }
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.data()
    }

    pub fn as_ptr(&self) -> *const T {
        self.data()
    }

    pub fn truncate(&mut self, len: usize) {
        let self_len = self.len();

        if len >= self_len {
            return;
        }

        self.raw.len = len;

        if !T::FINALIZE {
            return;
        }

        let s = unsafe { std::slice::from_raw_parts_mut(self.data().add(len), self_len - len) };
        for item in s {
            item.finalize();
        }
    }

    pub fn with_capacity(heap: &mut Heap, capacity: usize) -> Self {
        let capacity = if capacity == 0 {
            next_capacity::<T>(0)
        } else {
            capacity
        };

        let mut buf = unsafe { heap.varsize::<Raw<T>>(capacity).assume_init() };

        buf.len = 0;

        Self { raw: buf }
    }

    pub fn new(heap: &mut Heap) -> Self {
        Self::with_capacity(heap, 0)
    }

    pub fn push(&mut self, heap: &mut Heap, value: T) -> &mut T {
        let (len, capacity) = (self.len(), self.capacity());

        if len == capacity {
            self.grow(heap, next_capacity::<T>(capacity));
        }

        let len = self.len();
        let data = self.data();
        unsafe {
            data.add(len).write(value);
        }

        self.raw.len += 1;

        unsafe { &mut *data.add(len) }
    }

    pub fn try_push(&mut self, value: T) -> Option<&mut T> {
        let (len, capacity) = (self.len(), self.capacity());

        if len == capacity {
            return None;
        }

        let len = self.len();
        let data = self.data();
        unsafe {
            data.add(len).write(value);
        }

        self.raw.len += 1;

        Some(unsafe { &mut *data.add(len) })
    }

    pub fn pop(&mut self) -> Option<T> {
        let len = self.len();

        if len == 0 {
            return None;
        }

        let data = self.data();
        let value = unsafe { data.add(len - 1).read() };

        self.raw.len -= 1;

        Some(value)
    }

    pub fn try_reserve(&mut self, heap: &mut Heap, additional: usize) -> bool {
        let capacity = self.capacity();
        let total_required = self.len().saturating_add(additional);

        if total_required <= capacity {
            return true;
        }

        let mut new_capacity = next_capacity::<T>(capacity);

        while new_capacity < total_required {
            new_capacity = next_capacity::<T>(new_capacity);
        }

        if additional > max_elems::<T>() {
            return false;
        }

        self.grow(heap, new_capacity);

        true
    }

    pub fn reserve(&mut self, heap: &mut Heap, additional: usize) {
        if !self.try_reserve(heap, additional) {
            panic!("capacity overflow");
        }
    }

    pub fn append(&mut self, heap: &mut Heap, other: &mut Self) {
        if other.is_empty() {
            return;
        }

        let other_len = other.len();
        self.reserve(heap, other_len);

        unsafe {
            core::ptr::copy_nonoverlapping(
                other.as_ptr(),
                self.as_mut_ptr().add(self.len()),
                other_len,
            );
        }

        unsafe {
            other.set_len(0);
            self.set_len(self.len() + other_len);
        }
    }

    pub unsafe fn set_len(&mut self, len: usize) {
        self.raw.len = len;
    }

    pub fn clear(&mut self) {
        self.truncate(0);
    }

    pub fn remove(&mut self, index: usize) -> T {
        let len = self.len();

        assert!(
            (index < len),
            "removal index (is {}) should be < len (is {})",
            index,
            len
        );

        unsafe {
            let p = self.as_mut_ptr().add(index);

            let x = p.read();

            let src = p.add(1);
            let dst = p;
            let count = len - index - 1;
            core::ptr::copy(src, dst, count);

            self.set_len(len - 1);
            x
        }
    }

    pub fn remove_item<V>(&mut self, item: &V) -> Option<T> 
    where T: PartialEq<V> {
        let len = self.len();
        for i in 0..len {
            if self[i] == *item {
                return Some(self.remove(i));
            }
        }
        None
    }

    pub fn as_slice(&self) -> &[T] {
        &**self
    }

    pub fn as_slice_mut(&mut self) -> &mut [T] {
        &mut **self
    }
}

pub const fn next_capacity<T>(capacity: usize) -> usize {
    let elem_size = core::mem::size_of::<T>();

    if capacity == 0 {
        return match elem_size {
            1 => 8,
            2..=1024 => 4,
            _ => 1,
        };
    }

    capacity.saturating_mul(2)
}

pub const fn max_elems<T>() -> usize {
    let header_bytes = round_up(size_of::<Raw<T>>() as _, OBJECT_ALIGNMENT as _) as usize;
    let max = usize::MAX;

    let m = max - (max % OBJECT_ALIGNMENT) - header_bytes;

    m / size_of::<T>()
}

impl<T: Trace + Finalize + Allocation + 'static> Deref for ManagedVec<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        unsafe { std::slice::from_raw_parts(self.data(), self.len()) }
    }
}

impl<T: Trace + Finalize + Allocation + 'static> DerefMut for ManagedVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { std::slice::from_raw_parts_mut(self.data(), self.len()) }
    }
}


impl<T: Trace + Finalize + Allocation + 'static> Index<usize> for ManagedVec<T> {
    type Output = T;
    fn index(&self, index: usize) -> &Self::Output {
        assert!(index < self.len(), "index out of bounds: the len is {} but the index is {}", self.len(), index);

        unsafe { &*self.data().add(index) }
    }
}

impl<T: Trace + Finalize + Allocation + 'static> IndexMut<usize> for ManagedVec<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        assert!(index < self.len(), "index out of bounds: the len is {} but the index is {}", self.len(), index);

        unsafe { &mut *self.data().add(index) }
    }
}




impl<T: Trace + Finalize + Allocation + 'static + PartialEq> PartialEq for ManagedVec<T> {
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }

        for i in 0..self.len() {
            if self[i] != other[i] {
                return false;
            }
        }

        true
    }
}

impl<T: Trace + Finalize + Allocation + 'static + Eq> Eq for ManagedVec<T> {}

impl<T: Trace + Finalize + Allocation + 'static + PartialOrd> PartialOrd for ManagedVec<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Trace + Finalize + Allocation + 'static + Ord> Ord for ManagedVec<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<T: Trace + Finalize + Allocation + 'static + Hash> Hash for ManagedVec<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl<T: Trace + Finalize + Allocation + 'static + std::fmt::Debug> std::fmt::Debug for ManagedVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<T: Trace + Finalize + Allocation + 'static> AsRef<[T]> for ManagedVec<T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Trace + Finalize + Allocation + 'static> AsMut<[T]> for ManagedVec<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_slice_mut()
    }
}
