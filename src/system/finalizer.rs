use std::{mem::size_of, sync::atomic::AtomicPtr, collections::HashSet};

use once_cell::sync::Lazy;

use crate::{
    heap::heap::heap,
    prelude::{Handle, HeapObjectHeader, Object},
};

pub struct FinalizerQueue {
    list: AtomicPtr<Finalizer>,
}

impl FinalizerQueue {
    unsafe fn add(&self, object: *mut HeapObjectHeader, finalizer: unsafe fn (*mut u8)) {
        let finalizer = Box::new(Finalizer {
            next: std::ptr::null_mut(),
            object,
            finalizer,
        });

        let finalizer = Box::into_raw(finalizer);

        // CAS loop to append entry usign compare_exchange_weak
        let mut current = self.list.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            (*finalizer).next = current;
            match self.list.compare_exchange_weak(
                current,
                finalizer,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(next) => current = next,
            }
        }
    }

    /// Loop through entries, calling finalizers if the object is not alive anymore and update the list
    unsafe fn run(&self) {
        let heap = heap();

        let mut current = self.list.load(std::sync::atomic::Ordering::Relaxed);
        let mut prev = std::ptr::null_mut::<Finalizer>();
       
        while !current.is_null() {
            let next = (*current).next;

            if !heap.marking_context().is_marked((*current).object) {
                ((*current).finalizer)((*current).object.add(1).cast());

                // Remove entry from list
                if prev.is_null() {
                    self.list.store(next, std::sync::atomic::Ordering::Relaxed);
                } else {
                    (*prev).next = next;
                }

                // Free entry
                let _ = Box::from_raw(current);
            } else {
                prev = current;
            }

            current = next;
        }
    }
}

struct Finalizer {
    next: *mut Finalizer,
    object: *mut HeapObjectHeader,
    finalizer: unsafe fn (*mut u8),
}

static FINALIZE_QUEUE: Lazy<FinalizerQueue> = Lazy::new(|| FinalizerQueue {
    list: AtomicPtr::new(std::ptr::null_mut()),
});

pub(crate) fn register_for_finalization<T: Object>(
    object: Handle<T>,
) -> bool {
    unsafe {
        let object = object
            .as_ptr()
            .sub(size_of::<HeapObjectHeader>())
            .cast::<HeapObjectHeader>();
        debug_assert!(!(*object).finalization_ordering());

        (*object).set_finalization_ordering(true);

        unsafe fn finalizer<T>(object: *mut u8) {
            let object = object.cast::<T>();
            unsafe {
                core::ptr::drop_in_place(object);
            }
        }

        FINALIZE_QUEUE.add(
            object,
            finalizer::<T>
        );

        true
    }
}

pub(crate) fn finalize() {
    unsafe {
        FINALIZE_QUEUE.run();
    }
}