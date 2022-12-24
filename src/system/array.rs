use super::traits::*;
use crate::heap::thread::*;
use super::object::*;
use std::mem::size_of;
#[repr(C)]
pub struct Array<T: Object + Sized> {
    length: u32,
    init_length: u32,
    data: [T; 0],
}

impl<T: 'static + Object + Sized> Array<T> {
    pub fn new(th: &mut Thread, len: usize, mut init: impl FnMut(usize) -> T) -> Handle<Self> where T: Allocation {
        let mut result = th.allocate_varsize::<Self>(len);
        let arr = result.as_mut().as_mut_ptr();
        for i in 0..len {
            unsafe {
                let data = (*arr).data.as_mut_ptr().add(i);
                data.write(init(i));
                (*arr).init_length += 1;
            }
        }
        unsafe { result.assume_init() }
    }
}

impl<T: Object + ?Sized> Object for Handle<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.ptr.as_ptr());
    }
}

impl<T: Object> Object for Array<T> {
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
            let slice = std::slice::from_raw_parts(self.data.as_ptr().add(from), to - from);
            for val in slice.iter() {
                val.trace(visitor);
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

impl<T: Object + Sized + Allocation> Allocation for Array<T> {
    const FINALIZE: bool = std::mem::needs_drop::<T>();
    const DESTRUCTIBLE: bool = true;
    const SIZE: usize = size_of::<Self>();
    const NO_HEAP_PTRS: bool = true;
    const VARSIZE_NO_HEAP_PTRS: bool = T::NO_HEAP_PTRS;
    const VARSIZE: bool = true;
    const VARSIZE_ITEM_SIZE: usize = size_of::<T>();
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    const VARSIZE_OFFSETOF_VARPART: usize = size_of::<usize>();
}
