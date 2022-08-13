use crate::{memory::object_header::*, Managed};

use super::visitor::*;

/// Trait that specifies allocation behaviour.
pub unsafe trait Allocation: Trace + Finalize + Sized {
    /// If true then [T::finalize](Finalize::finalize) is invoked on the object when it is dead
    const FINALIZE: bool = core::mem::needs_drop::<Self>();
    /// If true then finalizer of this object cannot revive objects
    const LIGHT_FINALIZER: bool = Self::FINALIZE;
    /// Object statically known size.
    const SIZE: usize = core::mem::size_of::<Self>();

    /// If true then this object has GC pointers inside it, if false then it is not traced
    const HAS_GCPTRS: bool = true;

    /// If true object first field is weakptr. Used for [WeakRef][gcwrapper::WeakRef]
    const HAS_WEAKPTR: bool = false;

    /// Set to true when object is variably sized i.e arrays
    const VARSIZE: bool = false;
    /// Size of varsize object items
    const VARSIZE_ITEM_SIZE: usize = 0;
    /// Offset of length field in varsize object
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    /// Offset of variable part in varsize object
    const VARSIZE_OFFSETOF_VARPART: usize = 0;

    const WEAKREF_OFFSETS: &'static [usize] = &[];

    #[doc(hidden)]
    const __IS_WEAKMAP: bool = false;

    fn field_might_be_object(_: *mut u8) -> *mut ObjectHeader {
        unimplemented!("array object does not have such check implemented")
    }
}

pub fn finalize_erased<T: Finalize>(ptr: *mut ()) {
    unsafe {
        (&mut *ptr.cast::<T>()).finalize();
    }
}

pub(crate) fn weak_map_process_erased<T: Finalize>(ptr: *mut (), callback: &mut dyn FnMut(*mut ObjectHeader) -> *mut ObjectHeader) {
    unsafe {
        (&mut *ptr.cast::<T>()).__weak_map_process(callback);
    }
}

pub fn trace_erased<T: Trace>(ptr: *mut (), visitor: &mut dyn Visitor) {
    unsafe {
        let value = ptr.cast::<T>();

        (&mut *value).trace(visitor);
    }
}

pub unsafe trait Trace {
    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }
}

pub unsafe trait Finalize {
    fn finalize(&mut self) {
        unsafe {
            core::ptr::drop_in_place(self);
        }
    }

    #[doc(hidden)]
    fn __weak_map_process(&mut self, callback: &mut dyn FnMut(*mut ObjectHeader) -> *mut ObjectHeader) {
        let _ = callback;
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
            visitor.visit_pointer(self.header.as_ptr());
        }
    }
}

unsafe impl<T: ?Sized + ManagedObject> Finalize for Managed<T> {}
impl<T: ?Sized + ManagedObject> ManagedObject for Managed<T> {}

impl<K: Trace, V: Trace, S> ManagedObject for std::collections::HashMap<K, V, S> {}
unsafe impl<K: Trace, V: Trace, S> Allocation for std::collections::HashMap<K, V, S> {
    const HAS_GCPTRS: bool = true;
}

unsafe impl<K: Trace, V: Trace, S> Trace for std::collections::HashMap<K, V, S> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        for (key, value) in self.iter() {
            key.trace(visitor);
            value.trace(visitor);
        }
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