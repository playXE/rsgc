use crate::thread::Thread;
use atomic::Ordering;
use std::{
    marker::PhantomData,
    ptr::{null_mut, NonNull},
    sync::atomic::AtomicPtr,
};

use super::{
    object::{Allocation, Handle},
    traits::Object,
};

static mut HEAD_REF: AtomicPtr<u8> = AtomicPtr::new(null_mut());
unsafe impl<T: ?Sized + Object> Send for WeakReference<T> {}
unsafe impl<T: ?Sized + Object> Sync for WeakReference<T> {}

/// This structs implements weak references.
///
/// When a new weak references is allocated it is atomically linked
/// onto a singly linked list of weak references.
///
/// During a garbage collection this chain is traversed and references
/// are updated or cleared.
///
/// There are two modes of processing weak references.
///
///     1. Ignoring 'Long' References that track resurrection and
///        only updating references that should be cleared when the
///        object is finalized.
///     2. Updating all reference types.
///    
/// In any collection at least the second method must be called to
/// ensure no dangling pointers are created.
pub struct WeakReference<T: ?Sized + Object> {
    ptr: *mut u8,
    next: AtomicPtr<u8>,
    _marker: PhantomData<T>,
}

impl<T: ?Sized + Object> WeakReference<T> {
    pub fn new(thread: &mut Thread, target: Handle<T>) -> Handle<WeakReference<T>>
    where
        T: 'static,
    {
        unsafe {
            let this = thread.allocate_fixed(WeakReference {
                ptr: target.as_ptr(),
                next: AtomicPtr::new(null_mut()),
                _marker: PhantomData,
            });

            let this_ptr = this.as_ptr();
            let mut old_val = HEAD_REF.load(Ordering::Relaxed);
            loop {
                this.next.store(old_val, Ordering::Relaxed);
                match HEAD_REF.compare_exchange_weak(
                    old_val,
                    this_ptr,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(val) => old_val = val,
                }
            }

            this
        }
    }

    pub(crate) unsafe fn process(mut update_reference: impl FnMut(*mut u8) -> *mut u8) {
        let mut wr = HEAD_REF.load(Ordering::Relaxed);
        let mut tail = null_mut::<WeakReference<dyn Object>>();

        while !wr.is_null() {
            let weak = wr as *mut WeakReference<dyn Object>;
            let next = (*weak).next.load(Ordering::Relaxed);
            wr = update_reference(wr);
            if wr.is_null() {
                wr = next;
                if !tail.is_null() {
                    (*tail).next = AtomicPtr::new(wr);
                }
                continue;
            } else {
                let target = update_reference((*weak).ptr);

                (*weak).ptr = target;

                if tail.is_null() {
                    HEAD_REF.store(wr, Ordering::Relaxed);
                } else {
                    (*tail).next = AtomicPtr::new(wr);
                }

                tail = weak;
                wr = next;
            }
        }
    }

    pub fn upgrade(&self) -> Option<Handle<T>> {
        unsafe {
            let ptr = self.ptr;
            if ptr.is_null() {
                return None;
            }

            Some(Handle::from_raw(ptr))
        }
    }
}

impl<T: ?Sized + Object> Object for WeakReference<T> {}
impl<T: ?Sized + Object> Allocation for WeakReference<T> {}

pub struct WeakMapping<K: ?Sized + Object, V: ?Sized + Object> {
    key: *mut u8,
    value: Handle<V>,
    next: AtomicPtr<u8>,
    marker: PhantomData<K>,
}
