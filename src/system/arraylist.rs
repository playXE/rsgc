use core::fmt;
use std::{
    mem::{size_of, MaybeUninit},
    ops::{
        Deref, DerefMut, Index, IndexMut, Range, RangeFrom, RangeFull, RangeInclusive, RangeTo,
        RangeToInclusive,
    },
    sync::atomic::AtomicU32,
};

use memoffset::offset_of;
use crate::{
    needs_write_barrier,
    system::{
        object::{Allocation, Handle},
        traits::Object,
    },
    thread::Thread,
};

#[repr(C)]
struct RawArray<T: Object> {
    cap: AtomicU32,
    len: AtomicU32,
    data: [MaybeUninit<T>; 0],
}

impl<T: Object> RawArray<T> {
    fn new(th: &mut Thread, cap: usize) -> Handle<Self>
    where
        T: Allocation,
    {
        let mut result = th.allocate_varsize::<Self>(cap);
        let arr = result.as_mut().as_mut_ptr();
        unsafe {
            (*arr)
                .cap
                .store(cap as u32, std::sync::atomic::Ordering::Relaxed);
            (*arr).len.store(0, std::sync::atomic::Ordering::Relaxed);
        }
        unsafe { result.assume_init() }
    }


    fn as_slice(&self) -> &[MaybeUninit<T>] {
        unsafe {
            std::slice::from_raw_parts(
                self.data.as_ptr(),
                self.cap.load(std::sync::atomic::Ordering::Relaxed) as usize,
            )
        }
    }

    fn as_slice_mut(&mut self) -> &mut [MaybeUninit<T>] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.data.as_mut_ptr(),
                self.cap.load(std::sync::atomic::Ordering::Relaxed) as usize,
            )
        }
    }

    fn len(&self) -> usize {
        self.len.load(std::sync::atomic::Ordering::Relaxed) as usize
    }

    fn cap(&self) -> usize {
        self.cap.load(std::sync::atomic::Ordering::Relaxed) as usize
    }

    fn set_len(&self, len: usize) {
        self.len
            .store(len as u32, std::sync::atomic::Ordering::Relaxed);
    }

    #[allow(dead_code)]
    fn set_cap(&self, cap: usize) {
        self.cap
            .store(cap as u32, std::sync::atomic::Ordering::Relaxed);
    }
}

unsafe impl<T: Object> Object for RawArray<T> {
    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn crate::system::traits::Visitor) {
        unsafe {
            visitor.visit_conservative(self.data.as_ptr().add(from).cast(), to - from);
        }
    }
}

unsafe impl<T: Object + Allocation> Allocation for RawArray<T> {
    const VARSIZE: bool = true;
    const NO_HEAP_PTRS: bool = false;
    const VARSIZE_ITEM_SIZE: usize = size_of::<T>();
    const VARSIZE_NO_HEAP_PTRS: bool = false;
    const SIZE: usize = size_of::<Self>();
    const VARSIZE_OFFSETOF_CAPACITY: usize = offset_of!(Self, cap);
    const VARSIZE_OFFSETOF_LENGTH: usize = offset_of!(Self, len);
    const VARSIZE_OFFSETOF_VARPART: usize = offset_of!(Self, data);
}

/// Garbage collected array list. Based on [std::vec::Vec], supports negative indexing.
///
/// Write barriers are automatically inserted when usin [ArrayList::set] or [ArrayList::push]. If you
/// take mutable reference to this list and change. it in any way, you should insert write barriers manually.
#[repr(C)]
pub struct ArrayList<T: Object + Allocation> {
    array: Handle<RawArray<T>>,
}

impl<T: Object + Allocation> ArrayList<T> {
    /// Returns the maximum number of elements that can be stored in an ArrayList.
    ///
    /// It is equal to [u32::MAX] because at the moment RSGC only supports 32-bit lengths
    /// for arrays
    pub const fn max_elems() -> usize {
        u32::MAX as usize
    }

    pub fn array_offset() -> usize {
        offset_of!(Self, array)
    }

    pub fn cap_offset() -> usize {
        offset_of!(RawArray::<T>, cap)
    }

    pub fn len_offset() -> usize {
        offset_of!(RawArray::<T>, len)
    }

    pub fn data_offset() -> usize {
        offset_of!(RawArray::<T>, data)
    }

    pub fn with_capacity(thread: &mut Thread, capacity: usize) -> Self {
        Self {
            array: RawArray::new(thread, capacity),
        }
    }

    pub fn new(thread: &mut Thread) -> Self {
        Self::with_capacity(thread, 0)
    }

    pub fn from_slice(thread: &mut Thread, slice: &[T]) -> Self 
    where T: Clone
    {
        let mut this = Self::with_capacity(thread, slice.len());
        for val in slice {
            this.push(thread, val.clone());
        }
        this
    }

    pub fn from_slice_with_capacity(thread: &mut Thread, slice: &[T], capacity: usize) -> Self 
    where T: Clone
    {
        let mut this = Self::with_capacity(thread, slice.len().max(capacity));
        for val in slice {
            this.push(thread, val.clone());
        }
        this
    }

    pub fn from_iter(thread: &mut Thread, iter: impl IntoIterator<Item = T>) -> Self {
        let mut this = Self::new(thread);
        for val in iter {
            this.push(thread, val);
        }
        this
    }

    pub fn from_init(thread: &mut Thread, len: usize, init: T) -> Self
    where
        T: Clone,
    {
        let mut this = Self::with_capacity(thread, len);
        for _ in 0..len {
            this.push(thread, init.clone());
        }
        this
    }

    pub fn from_init_with_capacity(thread: &mut Thread, len: usize, init: T, capacity: usize) -> Self
    where
        T: Clone,
    {
        let mut this = Self::with_capacity(thread, len.max(capacity));
        for _ in 0..len {
            this.push(thread, init.clone());
        }
        this
    }

    pub fn split_off(&mut self, thread: &mut Thread, at: usize) -> Self {
        let mut other = Self::with_capacity(thread, self.len() - at);
        for i in at..self.len() {
            let val = self.remove(thread, i);
            other.push(thread, val.unwrap());
        }
        other
    }

    pub fn with(
        thread: &mut Thread,
        mut capacity: usize,
        len: usize,
        mut init: impl FnMut(&mut Thread, usize) -> T,
    ) -> Self {
        if capacity < len {
            capacity = len;
        }
        let mut this = Self::with_capacity(thread, capacity);

        for i in 0..len {
            let value = init(thread, i);
            this.push(thread, value);
        }

        this
    }

    pub fn capacity(&self) -> usize {
        self.array.as_ref().cap()
    }

    pub fn len(&self) -> usize {
        self.array.as_ref().len()
    }

    #[cold]
    fn grow(&mut self, thread: &mut Thread, new_cap: usize) {
        let old_cap = self.capacity();

        if old_cap == new_cap {
            return;
        }

        let len = self.len();

        let mut new_buf = RawArray::new(thread, new_cap);

        // no write-barrier needed with black mutator: new_buf is already black when allocated
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.array.as_ref().as_slice().as_ptr(),
                new_buf.as_slice_mut().as_mut_ptr(),
                len,
            );
        }
        self.array.set_len(0);
        new_buf.set_len(len);
        self.array = new_buf;
    }

    pub fn write_barrier(&mut self, thread: &mut Thread) {
        thread.write_barrier(self.array);
    }

    pub fn push(&mut self, thread: &mut Thread, val: T) {
        let len = self.len();
        if len == self.capacity() {
            self.grow(thread, next_capacity::<T>(self.capacity()));
        }

        // no need to do write barrier if T does not contain any heap pointers
        if needs_write_barrier::<T>() {
            thread.write_barrier(self.array);
        }
        unsafe {
            self.array
                .data
                .as_mut_ptr()
                .add(len)
                .write(MaybeUninit::new(val));
        }
        self.array.as_mut().set_len(len + 1);
    }

    pub fn pop(&mut self) -> Option<T> {
        let len = self.len();
        if len == 0 {
            return None;
        }

        let val = unsafe {
            self.array
                .as_ref()
                .data
                .as_ptr()
                .add(len - 1)
                .read()
                .assume_init_read()
        };
        self.array.as_mut().set_len(len - 1);
        Some(val)
    }

    pub fn get(&self, idx: usize) -> Option<&T> {
        if idx >= self.len() {
            return None;
        }

        unsafe { Some(&*self.array.as_ref().as_slice()[idx].as_ptr()) }
    }

    pub fn set(&mut self, thread: &mut Thread, idx: usize, val: T) -> Option<T> {
        if idx >= self.len() {
            return None;
        }

        //if needs_write_barrier::<T>() {
            thread.write_barrier(self.array);
        //}
        let old_val = unsafe { self.array.as_ref().as_slice()[idx].assume_init_read() };
        self.array.as_mut().as_slice_mut()[idx] = MaybeUninit::new(val);
        Some(old_val)
    }

    pub fn reverse(&self, thread: &mut Thread) -> Self
    where
        T: Clone,
    {
        let len = self.len();
        let mut new = Self::with_capacity(thread, len);
        for i in (0..len).rev() {
            new.push(thread, self[i].clone());
        }
        new
    }

    pub fn as_mut_ptr_range(&mut self) -> std::ops::Range<*mut T> {
        let len = self.len();
        let ptr = self.array.as_mut().as_slice_mut().as_mut_ptr().cast::<T>();
        unsafe { ptr..ptr.add(len) }
    }

    /// Reverses the order of elements in the slice, in place.
    pub fn reverse_in_place(&mut self) {
        let half_len = self.len() / 2;
        let Range { start, end } = self.as_mut_ptr_range();

        let (front_half, back_half) = // SAFETY: Both are subparts of the original slice, so the memory
        // range is valid, and they don't overlap because they're each only
        // half (or less) of the original slice.
        unsafe {
            (
                std::slice::from_raw_parts_mut(start, half_len),
                std::slice::from_raw_parts_mut(end.sub(half_len), half_len),
            )
        };

        revswap(front_half, back_half, half_len);

        #[inline]
        fn revswap<T>(a: &mut [T], b: &mut [T], n: usize) {
            debug_assert!(a.len() == n);
            debug_assert!(b.len() == n);

            // Because this function is first compiled in isolation,
            // this check tells LLVM that the indexing below is
            // in-bounds.  Then after inlining -- once the actual
            // lengths of the slices are known -- it's removed.
            let (a, b) = (&mut a[..n], &mut b[..n]);

            let mut i = 0;
            while i < n {
                std::mem::swap(&mut a[i], &mut b[n - 1 - i]);
                i += 1;
            }
        }
    }

    pub fn remove(&mut self, thread: &mut Thread, idx: usize) -> Option<T> {
        let len = self.len();
        if idx >= len {
            return None;
        }

        let val = unsafe { self.array.as_ref().as_slice()[idx].assume_init_read() };

        if needs_write_barrier::<T>() {
            thread.write_barrier(self.array);
        }
        unsafe {
            std::ptr::copy(
                self.array.as_ref().as_slice()[idx + 1..].as_ptr(),
                self.array.as_mut().as_slice_mut()[idx..].as_mut_ptr(),
                len - idx - 1,
            );
        }
        self.array.as_mut().set_len(len - 1);
        Some(val)
    }

    pub fn at(&self, idx: usize) -> &T {
        assert!(
            idx < self.len(),
            "index out of bounds: {} >= {}",
            idx,
            self.len()
        );
        unsafe { &*self.array.as_ref().as_slice()[idx].as_ptr() }
    }

    pub fn slice(&self, start: usize, end: usize) -> &[T] {
        assert!(start <= end, "start > end");
        assert!(end <= self.len(), "end > len");
        unsafe {
            &*(&self.array.as_ref().as_slice()[start..end] as *const [MaybeUninit<T>]
                as *const [T])
        }
    }

    pub fn slice_mut(&mut self, start: usize, end: usize) -> &mut [T] {
        assert!(start <= end, "start > end");
        assert!(end <= self.len(), "end > len");
        unsafe {
            &mut *(&mut self.array.as_mut().as_slice_mut()[start..end] as *mut [MaybeUninit<T>]
                as *mut [T])
        }
    }

    /// `try_reserve` attempts to reserve space for at least `additional` elements, returning a `bool` indicating if
    /// the allocation was succesful.
    ///
    /// Returns false if the allocation would exceed the maximum number of elements for this type (See [ArrayList::max_elems]).
    pub fn try_reserve(&mut self, thread: &mut Thread, additional: usize) -> bool {
        let capacity = self.capacity();
        let total_required = self.len().saturating_add(additional);

        if total_required <= capacity {
            return true;
        }

        let mut new_capacity = next_capacity::<T>(capacity);
        while new_capacity < total_required {
            new_capacity = next_capacity::<T>(new_capacity);
        }

        let max_elems = Self::max_elems();

        if !self.is_empty() && total_required > max_elems {
            return false;
        }

        if additional > max_elems {
            new_capacity = max_elems;
        }

        self.grow(thread, new_capacity);

        true
    }

    pub fn reserve(&mut self, thread: &mut Thread, additional: usize) {
        if !self.try_reserve(thread, additional) {
            panic!("allocation failed: capacity overflow");
        }
    }

    pub fn truncate(&mut self, len: usize) {
        let old_len = self.len();
        if len >= old_len {
            return;
        }

        self.array.as_mut().set_len(len);
    }

    pub fn swap_remove(&mut self, thread: &mut Thread, idx: usize) -> T {
        let len = self.len();
        assert!(idx < len, "index out of bounds: {} >= {}", idx, len);

        if needs_write_barrier::<T>() {
            thread.write_barrier(self.array);
        }

        let last = unsafe { self.array.as_ref().as_slice()[len - 1].assume_init_read() };
        let val = unsafe { self.array.as_ref().as_slice()[idx].assume_init_read() };
        self.array.as_mut().as_slice_mut()[idx] = MaybeUninit::new(last);
        self.array.as_mut().set_len(len - 1);
        val
    }

    pub fn resize(&mut self, thread: &mut Thread, new_len: usize, value: T)
    where
        T: Clone,
    {
        let len = self.len();
        if new_len > len {
            self.reserve(thread, new_len - len);
            for _ in len..new_len {
                self.push(thread, value.clone());
            }
        } else if new_len < len {
            self.truncate(new_len);
        }
    }

    pub fn clear(&mut self) {
        self.truncate(0);
    }

    pub fn resize_with(&mut self, thread: &mut Thread, new_len: usize, mut f: impl FnMut() -> T) {
        let len = self.len();
        if new_len > len {
            self.reserve(thread, new_len - len);
            for _ in len..new_len {
                self.push(thread, f());
            }
        } else if new_len < len {
            self.truncate(new_len);
        }
    }

    pub unsafe fn set_len(&mut self, len: usize) {
        self.array.as_mut().set_len(len);
    }

    pub fn retain_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut T) -> bool,
    {
        let len = self.len();

        let data = self.as_mut_ptr();

        let mut read = data;
        let mut write = read;

        let last = unsafe { data.add(len) };

        while read < last {
            let should_retain = unsafe { f(&mut *read) };
            if should_retain {
                if read != write {
                    unsafe {
                        core::mem::swap(&mut *read, &mut *write);
                    }
                }
                write = unsafe { write.add(1) };
            }

            read = unsafe { read.add(1) };
        }

        self.truncate((write as usize - data as usize) / core::mem::size_of::<T>());
    }

    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&T) -> bool,
    {
        let len = self.len();

        let data = self.as_mut_ptr();

        let mut read = data;
        let mut write = read;

        let last = unsafe { data.add(len) };

        while read < last {
            let should_retain = unsafe { f(&mut *read) };
            if should_retain {
                if read != write {
                    unsafe {
                        core::mem::swap(&mut *read, &mut *write);
                    }
                }
                write = unsafe { write.add(1) };
            }

            read = unsafe { read.add(1) };
        }

        self.truncate((write as usize - data as usize) / core::mem::size_of::<T>());
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.array.as_mut().as_slice_mut().as_mut_ptr() as *mut T
    }

    pub fn as_ptr(&self) -> *const T {
        self.array.as_ref().as_slice().as_ptr() as *const T
    }
}

impl<T: Object + Allocation> Index<usize> for ArrayList<T> {
    type Output = T;

    fn index(&self, idx: usize) -> &Self::Output {
        self.at(idx)
    }
}

impl<T: Object + Allocation> IndexMut<usize> for ArrayList<T> {
    fn index_mut(&mut self, idx: usize) -> &mut Self::Output {
        unsafe { &mut *self.array.as_mut().as_slice_mut()[idx].as_mut_ptr() }
    }
}

impl<T: Object + Allocation> Index<i32> for ArrayList<T> {
    type Output = T;

    fn index(&self, idx: i32) -> &Self::Output {
        let index = if idx < 0 {
            self.len() - (-idx) as usize
        } else {
            idx as usize
        };
        self.at(index)
    }
}

impl<T: Object + Allocation> IndexMut<i32> for ArrayList<T> {
    fn index_mut(&mut self, idx: i32) -> &mut Self::Output {
        let index = if idx < 0 {
            self.len() - (-idx) as usize
        } else {
            idx as usize
        };
        unsafe { &mut *self.array.as_mut().as_slice_mut()[index].as_mut_ptr() }
    }
}

impl<T: Object + Allocation> Index<Range<usize>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: Range<usize>) -> &Self::Output {
        self.slice(range.start, range.end)
    }
}

impl<T: Object + Allocation> IndexMut<Range<usize>> for ArrayList<T> {
    fn index_mut(&mut self, range: Range<usize>) -> &mut Self::Output {
        self.slice_mut(range.start, range.end)
    }
}

impl<T: Object + Allocation> Index<RangeFrom<usize>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeFrom<usize>) -> &Self::Output {
        self.slice(range.start, self.len())
    }
}

impl<T: Object + Allocation> IndexMut<RangeFrom<usize>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeFrom<usize>) -> &mut Self::Output {
        self.slice_mut(range.start, self.len())
    }
}

impl<T: Object + Allocation> Index<RangeTo<usize>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeTo<usize>) -> &Self::Output {
        self.slice(0, range.end)
    }
}

impl<T: Object + Allocation> IndexMut<RangeTo<usize>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeTo<usize>) -> &mut Self::Output {
        self.slice_mut(0, range.end)
    }
}

impl<T: Object + Allocation> Index<RangeInclusive<usize>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeInclusive<usize>) -> &Self::Output {
        self.slice(*range.start(), *range.end() + 1)
    }
}

impl<T: Object + Allocation> IndexMut<RangeInclusive<usize>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeInclusive<usize>) -> &mut Self::Output {
        self.slice_mut(*range.start(), *range.end() + 1)
    }
}

impl<T: Object + Allocation> Index<RangeToInclusive<usize>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeToInclusive<usize>) -> &Self::Output {
        self.slice(0, range.end + 1)
    }
}

impl<T: Object + Allocation> IndexMut<RangeToInclusive<usize>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeToInclusive<usize>) -> &mut Self::Output {
        self.slice_mut(0, range.end + 1)
    }
}

impl<T: Object + Allocation> Index<RangeFull> for ArrayList<T> {
    type Output = [T];

    fn index(&self, _range: RangeFull) -> &Self::Output {
        self.slice(0, self.len())
    }
}

impl<T: Object + Allocation> IndexMut<RangeFull> for ArrayList<T> {
    fn index_mut(&mut self, _range: RangeFull) -> &mut Self::Output {
        self.slice_mut(0, self.len())
    }
}
impl<T: Object + Allocation> Index<Range<i32>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: Range<i32>) -> &Self::Output {
        let start = if range.start < 0 {
            self.len() - (-range.start) as usize
        } else {
            range.start as usize
        };
        let end = if range.end < 0 {
            self.len() - (-range.end) as usize
        } else {
            range.end as usize
        };
        self.slice(start, end)
    }
}

impl<T: Object + Allocation> IndexMut<Range<i32>> for ArrayList<T> {
    fn index_mut(&mut self, range: Range<i32>) -> &mut Self::Output {
        let start = if range.start < 0 {
            self.len() - (-range.start) as usize
        } else {
            range.start as usize
        };
        let end = if range.end < 0 {
            self.len() - (-range.end) as usize
        } else {
            range.end as usize
        };
        self.slice_mut(start, end)
    }
}

impl<T: Object + Allocation> Index<RangeFrom<i32>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeFrom<i32>) -> &Self::Output {
        let start = if range.start < 0 {
            self.len() - (-range.start) as usize
        } else {
            range.start as usize
        };
        self.slice(start, self.len())
    }
}

impl<T: Object + Allocation> IndexMut<RangeFrom<i32>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeFrom<i32>) -> &mut Self::Output {
        let start = if range.start < 0 {
            self.len() - (-range.start) as usize
        } else {
            range.start as usize
        };
        self.slice_mut(start, self.len())
    }
}

impl<T: Object + Allocation> Index<RangeTo<i32>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeTo<i32>) -> &Self::Output {
        let end = if range.end < 0 {
            self.len() - (-range.end) as usize
        } else {
            range.end as usize
        };
        self.slice(0, end)
    }
}

impl<T: Object + Allocation> IndexMut<RangeTo<i32>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeTo<i32>) -> &mut Self::Output {
        let end = if range.end < 0 {
            self.len() - (-range.end) as usize
        } else {
            range.end as usize
        };
        self.slice_mut(0, end)
    }
}

impl<T: Object + Allocation> Index<RangeInclusive<i32>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeInclusive<i32>) -> &Self::Output {
        let start = if *range.start() < 0 {
            self.len() - (-*range.start()) as usize
        } else {
            *range.start() as usize
        };
        let end = if *range.end() < 0 {
            self.len() - (-*range.end()) as usize
        } else {
            *range.end() as usize
        };
        self.slice(start, end + 1)
    }
}

impl<T: Object + Allocation> IndexMut<RangeInclusive<i32>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeInclusive<i32>) -> &mut Self::Output {
        let start = if *range.start() < 0 {
            self.len() - (-*range.start()) as usize
        } else {
            *range.start() as usize
        };
        let end = if *range.end() < 0 {
            self.len() - (-*range.end()) as usize
        } else {
            *range.end() as usize
        };
        self.slice_mut(start, end + 1)
    }
}

impl<T: Object + Allocation> Index<RangeToInclusive<i32>> for ArrayList<T> {
    type Output = [T];

    fn index(&self, range: RangeToInclusive<i32>) -> &Self::Output {
        let end = if range.end < 0 {
            self.len() - (-range.end) as usize
        } else {
            range.end as usize
        };
        self.slice(0, end + 1)
    }
}

impl<T: Object + Allocation> IndexMut<RangeToInclusive<i32>> for ArrayList<T> {
    fn index_mut(&mut self, range: RangeToInclusive<i32>) -> &mut Self::Output {
        let end = if range.end < 0 {
            self.len() - (-range.end) as usize
        } else {
            range.end as usize
        };
        self.slice_mut(0, end + 1)
    }
}

impl<T: Object + Allocation> Deref for ArrayList<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.slice(0, self.len())
    }
}

impl<T: Object + Allocation> DerefMut for ArrayList<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.slice_mut(0, self.len())
    }
}

unsafe impl<T: Object + Allocation> Object for ArrayList<T> {
    fn trace(&self, visitor: &mut dyn crate::system::traits::Visitor) {
        self.array.trace(visitor);
    }
}

unsafe impl<T: Object + Allocation> Allocation for ArrayList<T> {}

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

impl<T: fmt::Debug + Object + Allocation> fmt::Debug for ArrayList<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}