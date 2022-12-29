use crate::system::object::*;

pub trait Object: 'static {
    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }

    fn finalize(&mut self) {
        unsafe {
            core::ptr::drop_in_place(self);
        }
    }

    fn process_weak(&mut self, processor: &mut dyn WeakProcessor) {
        let _ = processor;
    }

    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn Visitor) {
        let _ = (from, to, visitor);
    }
}

pub trait Visitor {
    fn visit(&mut self, object: *const u8);
    fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize);

    fn visit_count(&self) -> usize;
}

pub trait WeakProcessor {
    fn process(&mut self, object: *const u8) -> *const u8;
}

macro_rules! impl_simple {
    ($($t: ty)*) => {
        $(
            impl Object for $t {}
            impl $crate::system::object::Allocation for $t {
                const NO_HEAP_PTRS: bool = true;
            }
        )*
    };
}

impl_simple!(
    bool 
    f32 f64 
    u8 u16 u32 u64 u128
    i8 i16 i32 i64 i128 
    isize usize 
);

impl<T: Object> Object for Option<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        if let Some(value) = self {
            value.trace(visitor);
        }
    }

    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn Visitor) {
        if let Some(value) = self {
            value.trace_range(from, to, visitor);
        }
    }
}

impl<T: Object + Allocation> Allocation for Option<T> {
    const DESTRUCTIBLE: bool = T::DESTRUCTIBLE;
    const FINALIZE: bool = T::FINALIZE;
    const NO_HEAP_PTRS: bool = T::NO_HEAP_PTRS;
    const VARSIZE_NO_HEAP_PTRS: bool = T::VARSIZE_NO_HEAP_PTRS;
    const VARSIZE: bool = T::VARSIZE;   
}