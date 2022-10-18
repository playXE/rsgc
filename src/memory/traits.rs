use std::{ptr::null, mem::size_of};

use crate::{memory::object_header::*, Managed};

use super::visitor::*;

/// Trait that specifies allocation behaviour.
pub unsafe trait Allocation: Trace + Finalize + Sized {
    /// User defined virtual-table. Might be used by runtimes to implement some kind of virtual dispatch 
    /// without consuming space in heap and instead use vtable slot from heap object header.
    const USER_VTABLE: *const () = null();
    /// If true then [T::finalize](Finalize::finalize) is invoked on the object when it is dead
    const FINALIZE: bool = core::mem::needs_drop::<Self>();
    /// If true then finalizer of this object cannot revive objects
    const LIGHT_FINALIZER: bool = Self::FINALIZE;
    /// Object statically known size.
    const SIZE: usize = core::mem::size_of::<Self>();
    /// If true then this object has GC pointers inside it, if false then it is not traced
    const HAS_GCPTRS: bool = true;
    /// If true object has weak fields inside it. [Allocation::WEAKREF_OFFSETS] is used later to properly 
    /// break weak pointers
    const HAS_WEAKPTR: bool = false;
    /// Set to true when object is variably sized i.e arrays
    const VARSIZE: bool = false;
    /// Size of varsize object items
    const VARSIZE_ITEM_SIZE: usize = 0;
    /// Offset of length field in varsize object
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    /// Offset of variable part in varsize object
    const VARSIZE_OFFSETOF_VARPART: usize = 0;
    /// Offset of weak reference fields in an object. 
    const WEAKREF_OFFSETS: &'static [usize] = &[];

    #[doc(hidden)]
    const __IS_WEAKMAP: bool = false;

    fn field_might_be_object(_: *mut u8) -> *mut ObjectHeader {
        unimplemented!("array object does not have such check implemented")
    }
}

pub extern "C" fn finalize_erased<T: Finalize>(ptr: *mut ()) {
    unsafe {
        (&mut *ptr.cast::<T>()).finalize();
    }
}

#[repr(C)]
pub struct WeakMapProcessor<'a> {
    pub callback: &'a mut dyn FnMut(*mut ObjectHeader) -> *mut ObjectHeader,
}

impl<'a> WeakMapProcessor<'a> {
    pub fn process(&mut self, obj: *mut ObjectHeader) -> *mut ObjectHeader {
        (self.callback)(obj)
    }
}

pub(crate) extern "C" fn weak_map_process_erased<T: Finalize>(
    ptr: *mut (),
    callback: &mut WeakMapProcessor,
) {
    unsafe {
        (&mut *ptr.cast::<T>()).__weak_map_process(callback);
    }
}

pub extern "C" fn trace_erased<T: Trace>(ptr: *mut (), visitor: &mut Tracer) {
    unsafe {
        let value = ptr.cast::<T>();

        (&mut *value).trace(&mut *visitor);
    }
}


/// Indicates that a type can be traced by a garbage collector.
///
/// This doesn't necessarily mean that the type is safe to allocate in a garbage collector ([Allocation]).
///
/// ## Safety
/// See the documentation of the `trace` method for more info.
/// Essentially, this object must faithfully trace anything that
/// could contain garbage collected pointers or other `Trace` items.
pub unsafe trait Trace {
    /// Trace each field in this type.
    ///
    /// Structures should trace each of their fields,
    /// and collections should trace each of their elements.
    ///
    /// ### Safety
    /// Some types (like `Managed`) need special actions taken when they're traced,
    /// but those are somewhat rare and are usually already provided by the garbage collector.
    ///
    /// Behavior is restricted during tracing:
    /// ## Permitted Behavior
    /// - Reading your own memory (includes iteration)
    ///   - Interior mutation is undefined behavior, even if you use `RefCell`
    /// - Calling `Visitor` methods for tracing
    /// - Panicking on unrecoverable errors
    ///   - This should be reserved for cases where you are seriously screwed up,
    ///       and can't fulfill your contract to trace your interior properly.
    ///     - One example is `Managed<T>` which panics if the garbage collectors are mismatched
    ///   - Garbage collectors may chose to [abort](std::process::abort) if they encounter a panic,
    ///     so you should avoid doing it if possible.
    /// ## Never Permitted Behavior
    /// - Forgetting a element of a collection, or field of a structure
    ///   - If you forget an element undefined behavior will result
    ///   - This is why you should always prefer automatically derived implementations where possible.
    ///     - With an automatically derived implementation you will never miss a field
    /// - It is undefined behavior to mutate any of your own data.
    /// - Calling other operations on the garbage collector (including allocations)
    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }
}

/// Indicates that a type can be finalized by a garbage collector.
/// 
/// # Safety
/// 
/// See the documentation of the `finalize` method for more info.
/// 
/// # Ordering of finalizers (based on MiniMark from RPython)
/// 
/// After a collection, the GC should call the finalizers on some of the objects that have one and that have become unreachable. Basically, if there is a reference chain from an object a to an object b then it should not call the finalizer for b immediately, but just keep b alive and try again to call its finalizer after the next collection.
///
/// (Note that this creates rare but annoying issues as soon as the program creates chains of objects with finalizers more quickly than the rate at collections go (which is very slow). It is still unclear what the real consequences would be on programs in the wild.)
///
/// The basic idea fails in the presence of cycles. It’s not a good idea to keep the objects alive forever or to never call any of the finalizers. The model we came up with is that in this case, we could just call the finalizer of one of the objects in the cycle – but only, of course, if there are no other objects outside the cycle that has a finalizer and a reference to the cycle.
///
/// More precisely, given the graph of references between objects:
/// ```python
/// for each strongly connected component C of the graph:
///    if C has at least one object with a finalizer:
///        if there is no object outside C which has a finalizer and
///        indirectly references the objects in C:
///            mark one of the objects of C that has a finalizer
///            copy C and all objects it references to the new space
///
/// for each marked object:
///    detach the finalizer (so that it's not called more than once)
///    call the finalizer
/// ```
pub unsafe trait Finalize {
    /// Finalize this object. Default behaviour is to just invoke [drop_in_place](core::ptr::drop_in_place).
    /// You can override this method to do some more complex cleanup. Finalize is not invoked if [`Allocation::FINALIZE`] is false.
    /// 
    /// # Safety
    /// 
    /// When object that implements this trait has [`Allocation::LIGHT_FINALIZER`] set to true finalizer must not:
    /// - Panic
    /// - Recover any GC objects and access GC references
    /// - Calling other operations on the garbage collector (including allocations). 
    /// 
    /// Essentially lightly finalizeable objects must not do anything that could potentially cause GC to run and their finalizers
    /// are mostly equal to [drop](core::ops::Drop::drop).
    /// 
    /// If [`Allocation::LIGHT_FINALIZER`] is false then you can do anything you want in finalizer. GC will invoke finalizers in a specific order.
    fn finalize(&mut self) {
        unsafe {
            core::ptr::drop_in_place(self);
        }
    }

    #[doc(hidden)]
    fn __weak_map_process(&mut self, processor: &mut WeakMapProcessor<'_>) {
        let _ = processor;
    }
}

pub trait ManagedObject: Trace + Finalize {}

macro_rules! impl_prim {
    ($($t:ty)*) => {
        $(
            unsafe impl Finalize for $t {}
            unsafe impl Trace for $t {}
            impl ManagedObject for $t {}

            unsafe impl Allocation for $t {
                const HAS_GCPTRS: bool = false;
                const FINALIZE: bool = false;
                const LIGHT_FINALIZER: bool = false;
            }
        )*
    };
}

impl_prim!(u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64 bool);

macro_rules! impl_light {
    ($($t:ty)*) => {$(
        unsafe impl Allocation for $t {
            const FINALIZE: bool = true;
            const LIGHT_FINALIZER: bool = true;
            const HAS_GCPTRS: bool = false;
        }

        unsafe impl Trace for $t {}
        unsafe impl Finalize for $t {}
        impl ManagedObject for $t {}
    )*
    };
}

impl_light!(String std::fs::File std::path::PathBuf);

unsafe impl<T: ?Sized + ManagedObject> Allocation for Managed<T> {
    const HAS_GCPTRS: bool = true;
    const HAS_WEAKPTR: bool = false;
    const VARSIZE: bool = false;
    const VARSIZE_ITEM_SIZE: usize = 0;
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    const VARSIZE_OFFSETOF_VARPART: usize = 0;
    const WEAKREF_OFFSETS: &'static [usize] = &[];
}

unsafe impl<T: ?Sized + ManagedObject> Trace for Managed<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        unsafe {
            visitor.visit_pointer(self.ptr.as_ptr().sub(size_of::<ObjectHeader>()).cast());
        }
    }
}

unsafe impl<T: ?Sized + ManagedObject> Finalize for Managed<T> {}
impl<T: ?Sized + ManagedObject> ManagedObject for Managed<T> {}

impl<K: Trace, V: Trace, S: Trace> ManagedObject for std::collections::HashMap<K, V, S> {}
unsafe impl<K: Trace, V: Trace, S: Trace> Allocation for std::collections::HashMap<K, V, S> {
    const HAS_GCPTRS: bool = true;
}

unsafe impl<K: Trace, V: Trace, S: Trace> Trace for std::collections::HashMap<K, V, S> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        for (key, value) in self.iter() {
            key.trace(visitor);
            value.trace(visitor);
        }
        self.hasher().trace(visitor);
    }
}

unsafe impl<K: Trace, V: Trace, S> Finalize for std::collections::HashMap<K, V, S> {}

unsafe impl<T: Trace> Trace for Option<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        match self {
            Some(value) => value.trace(visitor),
            None => (),
        }
    }
}
unsafe impl<T: Finalize> Finalize for Option<T> {}
unsafe impl<T: Allocation> Allocation for Option<T> {}

impl<T: ManagedObject> ManagedObject for Option<T> {}

macro_rules! impl_tuple {
    (
        $(
            (
                $($var: ident)*
            )
        ),*
    ) => {
        $(
            impl<$($var: ManagedObject),*> ManagedObject for ($($var,)*) {}
            unsafe impl<$($var: Allocation),*> Allocation for ($($var,)*) {
                const HAS_GCPTRS: bool = $($var::HAS_GCPTRS)||* || false;
                const LIGHT_FINALIZER: bool = $($var::LIGHT_FINALIZER)&&* || std::mem::needs_drop::<Self>();
                const FINALIZE: bool = $($var::FINALIZE)||* || std::mem::needs_drop::<Self>();
            }
            #[allow(non_snake_case)]
            unsafe impl<$($var: Trace),*> Trace for ($($var,)*) {
                fn trace(&self, visitor: &mut dyn Visitor) {
                    let ($($var,)*) = self;
                    $(
                        $var.trace(visitor);
                    )*
                    let _ = visitor;
                }
            }
            unsafe impl<$($var: Finalize),*> Finalize for ($($var,)*) {}
        )*
    };
}

impl_tuple!(
    (A),
    (A B),
    (A B C),
    (A B C D),
    (A B C D E),
    (A B C D E F),
    (A B C D E F G),
    (A B C D E F G H),
    (A B C D E F G H I),
    (A B C D E F G H I J),
    (A B C D E F G H I J K),
    (A B C D E F G H I J K L),
    (A B C D E F G H I J K L M),
    (A B C D E F G H I J K L M N),
    (A B C D E F G H I J K L M N O),
    (A B C D E F G H I J K L M N O P),
    (A B C D E F G H I J K L M N O P Q)
);

unsafe impl<const N: usize, T: Trace> Trace for [T; N] {
    fn trace(&self, visitor: &mut dyn Visitor) {
        for i in 0..N {
            self[i].trace(visitor);
        }
    }
}

unsafe impl<const N: usize, T: Finalize> Finalize for [T; N] {}

unsafe impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        self.iter().for_each(|value| value.trace(visitor));
    }
}
unsafe impl<T: Finalize> Finalize for Vec<T> {}

unsafe impl<T: Trace + Finalize> Allocation for Vec<T> {
    const FINALIZE: bool = true;
    const LIGHT_FINALIZER: bool = true;
    const HAS_WEAKPTR: bool = false;
}


unsafe impl<F> Trace for F where F: Fn(&mut dyn Visitor) {
    fn trace(&self, visitor: &mut dyn Visitor) {
        self(visitor);
    }
}


/// Persistent root is an object that will be alive as long as [is_live](PersistentRoot::is_live) returns true.
pub trait PersistentRoot {
    fn is_live(&self) -> bool;
    fn visit(&mut self, visitor: &mut dyn Visitor);
}