use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;
pub trait LockFreeItem: Sized {
    fn next_ptr(&self) -> &AtomicPtr<Self>;
}

pub struct LockFreeStack<T: LockFreeItem> {
    top: AtomicPtr<T>
}

impl<T: LockFreeItem> LockFreeStack<T> {
    pub fn top(&self) -> *mut T {
        self.top.load(Ordering::Relaxed)
    }

    unsafe fn prepend_impl(&self, first: *mut T, last: *mut T) {
        let mut cur = self.top();
        let mut old;
        loop {
            old = cur;
            Self::set_next(&*last, cur);

            cur = match self.top.compare_exchange(cur, first, Ordering::SeqCst, Ordering::Relaxed) {
                Ok(v) => v,
                Err(v) => v
            };

            if old == cur {
                break;
            }
        }
    }

    pub fn next(value: &T) -> *mut T {
        value.next_ptr().load(Ordering::Relaxed)
    }

    pub fn set_next(value: &T, next: *mut T) {
       value.next_ptr().store(next, Ordering::Relaxed);
    }

    pub const fn new() -> Self {
        Self {
            top: AtomicPtr::new(core::ptr::null_mut())
        }
    }

    pub fn pop_all(&self) -> *mut T {
        self.top.swap(core::ptr::null_mut(), Ordering::SeqCst)
    }

    pub unsafe fn push(&self, value: *mut T) {
        self.prepend_impl(value, value);
    }

    pub unsafe fn prepend_two(&self, first: *mut T, last: *mut T) {
        self.prepend_impl(first, last);
    }

    pub unsafe fn prepend(&self, first: *mut T) {
        let mut last = first;
        loop {
            let step_to = Self::next(&*last);
            if step_to.is_null() {
                break;
            }
            last = step_to;
        }

        self.prepend_impl(first, last);
    }

    pub fn empty(&self) -> bool {
        self.top.load(Ordering::Relaxed).is_null()
    }

    pub unsafe fn length(&self) -> usize {
        let mut cur = self.top();
        let mut len = 0;
        while !cur.is_null() {
            len += 1;
            cur = Self::next(&*cur);
        }
        len
    }

}