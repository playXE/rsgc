//! Common traits used by GC. 
use crate::system::object::*;


/// Indicates that a type can be traced by a garbage collector.
///
/// This doesn't necessarily mean that the type is safe to allocate in a garbage collector ([Allocation]).
/// 
/// ## Safety
/// See the documentation of the `trace` method for more info.
/// Essentially, this object must faithfully trace anything that
/// could contain garbage collected pointers or other `Trace` items.
pub trait Object: 'static {

    /// Trace each field in this type.
    ///
    /// Structures should trace each of their fields,
    /// and collections should trace each of their elements.
    ///
    /// ### Safety
    /// Some types (like `Handle`) need special actions taken when they're traced,
    /// but those are somewhat rare and are usually already provided by the garbage collector.
    ///
    /// Behavior is restricted during tracing:
    /// ## Permitted Behavior
    /// - Reading your own memory (includes iteration)
    ///   - Interior mutation is undefined behavior, even if you use `GcCell`
    /// - Calling `Visitor::visit`
    /// - Calling [Object::trace] on other objects
    /// ## Never Permitted Behavior
    /// - Forgetting a element of a collection, or field of a structure
    ///   - If you forget an element undefined behavior will result
    /// - It is undefined behavior to mutate any of your own data.
    /// - Calling other operations on the garbage collector (including allocations)
    /// - Panicking (this is a temporary restriction)
    ///   - GC does not currently support panicking during tracing. If you're seriously fucked up 
    ///     you have to use `std::process::abort` instead. 
    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }

    /// Left here for future use. 
    #[doc(hidden)]
    fn finalize(&mut self) {
        unsafe {
            core::ptr::drop_in_place(self);
        }
    }

    /// Invoked when the object has weak references. 
    /// 
    /// ## Safety
    /// 
    /// Same rules as in [Object::trace] method.
    fn process_weak(&mut self, processor: &mut dyn WeakProcessor) {
        let _ = processor;
    }

    /// Invoked to process array in chunks. 
    /// 
    /// ## Safety
    /// 
    /// Same rules as in [Object::trace] method.
    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn Visitor) {
        let _ = (from, to, visitor);
    }
}


/// Visits garbage collected objects
pub trait Visitor {
    /// Visit a pointer to a garbage collected object.
    fn visit(&mut self, object: *const u8);
    /// Visit range of pointers to garbage collected objects conservatively.
    fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize);

    /// Returns number of visited objects.
    fn visit_count(&self) -> usize;
}

/// Visits weak references to objects.
pub trait WeakProcessor {
    /// Process weak reference to a garbage collected object.
    /// 
    /// Returns new pointer to the object. If object is dead then null pointer is returned.
    /// Otherwise returned pointer must be equal to the original pointer.
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
    isize usize char std::fs::File
    std::net::TcpStream
    std::net::UdpSocket
    std::net::TcpListener
);

impl Object for fn() {}

impl<R: 'static> Object for fn() -> R {}
impl<A1: 'static, R: 'static> Object for fn(A1) -> R {}
impl<A1: 'static, A2: 'static, R: 'static> Object for fn(A1, A2) -> R {}
impl<A1: 'static, A2: 'static, A3: 'static, R: 'static> Object for fn(A1, A2, A3) -> R {}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, R: 'static> Object for fn(A1, A2, A3, A4) -> R {}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, R: 'static> Object for fn(A1, A2, A3, A4, A5) -> R {}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, R: 'static> Object for fn(A1, A2, A3, A4, A5, A6) -> R {}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, A7: 'static, R: 'static> Object for fn(A1, A2, A3, A4, A5, A6, A7) -> R {}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, A7: 'static, A8: 'static, R: 'static> Object for fn(A1, A2, A3, A4, A5, A6, A7, A8) -> R {}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, A7: 'static, A8: 'static, A9: 'static, R: 'static> Object for fn(A1, A2, A3, A4, A5, A6, A7, A8, A9) -> R {}
impl<R: 'static> Allocation for fn() -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, R: 'static> Allocation for fn(A1) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, R: 'static> Allocation for fn(A1, A2) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, A3: 'static, R: 'static> Allocation for fn(A1, A2, A3) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, R: 'static> Allocation for fn(A1, A2, A3, A4) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, R: 'static> Allocation for fn(A1, A2, A3, A4, A5) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, R: 'static> Allocation for fn(A1, A2, A3, A4, A5, A6) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, A7: 'static, R: 'static> Allocation for fn(A1, A2, A3, A4, A5, A6, A7) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, A7: 'static, A8: 'static, R: 'static> Allocation for fn(A1, A2, A3, A4, A5, A6, A7, A8) -> R {
    const NO_HEAP_PTRS: bool = true;
}
impl<A1: 'static, A2: 'static, A3: 'static, A4: 'static, A5: 'static, A6: 'static, A7: 'static, A8: 'static, A9: 'static, R: 'static> Allocation for fn(A1, A2, A3, A4, A5, A6, A7, A8, A9) -> R {
    const NO_HEAP_PTRS: bool = true;
}


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