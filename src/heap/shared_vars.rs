use std::sync::atomic::{AtomicU8, Ordering};

pub type SharedValue = AtomicU8;

pub struct SharedFlag(SharedValue);

impl SharedFlag {
    pub const UNSET: Self = Self(SharedValue::new(0));
    pub const SET: Self = Self(SharedValue::new(1));

    pub const fn new() -> Self {
        Self::UNSET 
    }

    pub fn set(&self) {
        self.0.store(1, Ordering::Release);
    }

    pub fn unset(&self) {
        self.0.store(0, Ordering::Release);
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire) == 1
    }

    pub fn is_unset(&self) -> bool {
        self.0.load(Ordering::Acquire) == 0
    }

    pub fn set_cond(&self, c: bool) {
        if c {
            self.set();
        } else {
            self.unset();
        }
    }

    pub fn try_set(&self) -> bool {
        if self.is_set() {
            return false;
        }

        match self.0.compare_exchange_weak(0, 1, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => true,
            Err(_) => false
        }
    }

    pub fn try_unset(&self) -> bool {
        if self.is_unset() {
            return false;
        }

        match self.0.compare_exchange_weak(1, 0, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => true,
            Err(_) => false
        }
    }

}

pub struct SharedEnumFlag(SharedValue);

impl SharedEnumFlag {
    pub fn new() -> Self {
        Self(SharedValue::new(0))
    }

    pub fn set(&self, val: u8) {
        self.0.store(val, Ordering::Release);
    }

    pub fn get(&self) -> u8 {
        self.0.load(Ordering::Acquire)
    }

    pub fn cmpxchg(&self, old: u8, new: u8) -> u8 {
        match self.0.compare_exchange_weak(old, new, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(val) => val,
            Err(val) => val
        }
    }

    pub fn addr_of(&self) -> *const SharedValue {
        &self.0
    }
}