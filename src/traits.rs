pub unsafe trait Allocation: Sized {
    const SIZE: usize = core::mem::size_of::<Self>();

    const VARSIZE: bool = false;
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    const VARSIZE_OFFSETOF_CAPACITY: usize = 0;
    const VARSIZE_ITEMSIZE: usize = 0;

    const WEAK_OFFSETS: Option<&'static [usize]> = None;
}

pub trait Visitor {
    unsafe fn visit(&mut self, _: *const u8);
    unsafe fn visit_range(&mut self, _: *const u8, _: *const u8);
    unsafe fn visit_range_conservatively(&mut self, _: *const u8, _: *const u8);
}

pub unsafe trait Object {
    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }
    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn Visitor) {
        let _ = (from, to, visitor);
    }
}

macro_rules! impl_simple {
    ($($t: ty)*) => {
        $(
            unsafe impl Object for $t {}
            unsafe impl Allocation for $t {}
        )*
    };
}

impl_simple!(bool f32 f64 u8 u16 u32 u64 usize i8 i16 i32 i64 isize);

unsafe impl<T: Object> Object for Option<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        if let Some(x) = self {
            x.trace(visitor);
        }
    }

    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn Visitor) {
        if let Some(x) = self {
            x.trace_range(from, to, visitor);
        }
    }
}

unsafe impl<T: Object> Allocation for Option<T> {}


