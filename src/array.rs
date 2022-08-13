use std::ops::{Deref, DerefMut};

use memoffset::offset_of;

use crate::{memory::traits::{Allocation, Finalize, ManagedObject, Trace}, Managed, WeakRef};

#[repr(C)]
pub struct Array<T: Trace> {
    length: usize,
    data: [T; 0],
}

impl<T: Trace> Array<T> {
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

unsafe impl<T: Allocation + Trace + IsManaged> Allocation for Array<T> {
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
    /// Array is traced if item is a managed object or item has managed objects inside it.
    const HAS_GCPTRS: bool = T::HAS_GCPTRS || T::IS_MANAGED;
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
unsafe impl<T: Trace + Finalize> Finalize for Array<T> {}
impl<T: Trace + Finalize> ManagedObject for Array<T> {}

pub trait IsManaged {
    const IS_MANAGED: bool;
}

impl<T: ManagedObject + ?Sized> IsManaged for Managed<T> {
    const IS_MANAGED: bool = true;
}

impl<T: ManagedObject + ?Sized> IsManaged for WeakRef<T> {
    const IS_MANAGED: bool = true;
}



default impl<T> IsManaged for T {
    const IS_MANAGED: bool = false;
}