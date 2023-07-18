use std::{sync::atomic::AtomicPtr, ptr::null_mut};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressRange {
    pub address_low: *const *const u8,
    pub address_high: *const *const u8,
}

impl AddressRange {
    #[inline]
    pub fn contains(&self, range: &AddressRange) -> bool {
        self.address_low <= range.address_low && self.address_high >= range.address_high
    }
}

pub struct GCRoots {
    range: AddressRange,
    next: AtomicPtr<GCRoots>,
}

static ROOTS: AtomicPtr<GCRoots> = AtomicPtr::new(std::ptr::null_mut());
static LOCK: parking_lot::Mutex<()> = parking_lot::const_mutex(());
pub fn gc_roots_add(range: AddressRange) {
    let lock = LOCK.lock();

    let roots = ROOTS.load(std::sync::atomic::Ordering::Relaxed);

    let node = Box::into_raw(Box::new(GCRoots {
        range,
        next: AtomicPtr::new(roots),
    }));

    ROOTS.store(node, std::sync::atomic::Ordering::Relaxed);
    drop(lock);
}

pub unsafe fn gc_roots_add_no_lock(range: AddressRange) {
    let roots = ROOTS.load(std::sync::atomic::Ordering::Relaxed);

    let node = Box::into_raw(Box::new(GCRoots {
        range,
        next: AtomicPtr::new(roots),
    }));

    ROOTS.store(node, std::sync::atomic::Ordering::Relaxed);
}

pub fn gc_roots_add_except(range: AddressRange, except: AddressRange) {

    if range.address_low < except.address_low {
        gc_roots_add(AddressRange {
            address_low: range.address_low,
            address_high: except.address_low,
        });
    }

    if range.address_high > except.address_high {
        gc_roots_add(AddressRange {
            address_low: except.address_high,
            address_high: range.address_high,
        });
    }
    
}

pub unsafe fn gc_roots_add_except_no_lock(range: AddressRange, except: AddressRange) {
    if range.address_low < except.address_low {
        gc_roots_add_no_lock(AddressRange {
            address_low: range.address_low,
            address_high: except.address_low,
        });
    }

    if range.address_high > except.address_high {
        gc_roots_add_no_lock(AddressRange {
            address_low: except.address_high,
            address_high: range.address_high,
        });
    }
}

pub fn gc_roots_remove_by_range(range: AddressRange) {
    let lock = LOCK.lock();
    unsafe {
        let mut current = ROOTS.load(std::sync::atomic::Ordering::Relaxed);
        let mut prev = null_mut::<GCRoots>();

        while !current.is_null() {
            if range.contains(&(*current).range) {
                let current_range = (*current).range;

                if prev.is_null() {
                    ROOTS.store((*current).next.load(std::sync::atomic::Ordering::Relaxed), std::sync::atomic::Ordering::Relaxed);
                } else {
                    (*prev).next.store((*current).next.load(std::sync::atomic::Ordering::Relaxed), std::sync::atomic::Ordering::Relaxed);
                }

                

                gc_roots_add_except_no_lock(current_range, range);
                prev = current;
                let next = (*current).next.load(std::sync::atomic::Ordering::Relaxed);
                drop(Box::from_raw(current));
                current = next;
            } else {
                prev = current;
                current = (*current).next.load(std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    drop(lock);
}