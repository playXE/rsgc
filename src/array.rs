use std::{ops::{Deref, DerefMut}, mem::MaybeUninit};

use memoffset::offset_of;

use crate::{memory::{traits::{Allocation, Finalize, ManagedObject, Trace}, Heap}, Managed};

/// Immutable array of `T` values allocated on the GC heap.
#[repr(C)]
pub struct Array<T: Trace> {
    length: usize,
    data: [T; 0],
}

impl<T: Trace> Array<T> {
    pub fn as_ptr(&self) -> *const T {
        self.data.as_ptr()
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.data.as_mut_ptr()
    }

    pub fn as_slice(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts(self.data.as_ptr(), self.length) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.data.as_mut_ptr(), self.length) }
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub unsafe fn uninit(heap: &mut Heap, length: usize) -> Managed<Array<MaybeUninit<T>>>
    where T: Allocation + 'static
    {
        let ptr = heap.varsize::<Self>(length);

        std::mem::transmute(ptr.assume_init())
    }

    pub fn new(heap: &mut Heap, length: usize, init: T) -> Managed<Array<T>>
    where T: Clone + Allocation + 'static
    {
        unsafe {
            let mut arr = Self::uninit(heap, length);

            for i in 0..length {
                arr.as_mut_ptr().add(i).cast::<T>().write(init.clone());
            }
            arr.assume_init()
        }
    }

    pub fn new_with(heap: &mut Heap, length: usize, mut init: impl FnMut(usize) -> T) -> Managed<Array<T>>
    where T: Allocation + 'static
    {
        unsafe {
            let mut arr = Self::uninit(heap, length);

            for i in 0..length {
                arr.as_mut_ptr().add(i).cast::<T>().write(init(i));
            }
            arr.assume_init()
        }
    }

}

impl<T: Trace + Finalize> Managed<Array<MaybeUninit<T>>> {
    pub unsafe fn assume_init(self) -> Managed<Array<T>> {
        std::mem::transmute(self)
    }
}


impl<T: Trace> AsRef<[T]> for Array<T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Trace> AsMut<[T]> for Array<T> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_slice_mut()
    }
}

impl<T: Trace> Deref for Array<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Trace> DerefMut for Array<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_slice_mut()
    }
}

unsafe impl<T: Allocation + Trace> Allocation for Array<T> {
    const FINALIZE: bool = T::FINALIZE;
    const LIGHT_FINALIZER: bool = {
        if T::FINALIZE {
            assert!(
                T::LIGHT_FINALIZER,
                "Array<T> cannot store types that require complex finalizers"
            );
        }
        true
    };
    const VARSIZE: bool = true;
    const VARSIZE_ITEM_SIZE: usize = T::SIZE;
    const VARSIZE_OFFSETOF_LENGTH: usize = offset_of!(Array::<T>, length);
    const VARSIZE_OFFSETOF_VARPART: usize = offset_of!(Array::<T>, data);
}

unsafe impl<T: Trace> Trace for Array<T> {
    fn trace(&self, visitor: &mut dyn crate::memory::visitor::Visitor) {
        for item in self.as_slice() {
            item.trace(visitor);
        }
    }
}
unsafe impl<T: Trace + Finalize> Finalize for Array<T> {
    fn finalize(&mut self) {
        let ptr = self.data.as_mut_ptr();
        for i in 0..self.length {
            unsafe {
                let item = ptr.add(i);
                (*item).finalize();
            }
        }
    }
}


impl<T: Trace + Finalize> ManagedObject for Array<T> {}

