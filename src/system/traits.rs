use crate::system::object::*;

pub trait Object {
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
            impl $crate::system::object::Allocation for $t {}
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
