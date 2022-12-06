use std::sync::atomic::{AtomicU64, Ordering};

pub trait BitFieldStorage<T> {
    fn from_value(value: T) -> Self;
    fn into_value(self) -> T;

    fn from_usize(value: usize) -> Self;
    fn into_usize(self) -> usize;
}

pub struct BitField<const SIZE: usize, const POSITION: usize, const SIGN_EXTEND: bool>;

impl<const SIZE: usize, const POSITION: usize, const SIGN_EXTEND: bool>
    BitField<SIZE, POSITION, SIGN_EXTEND>
{
    pub const NEXT_BIT: usize = POSITION + SIZE;
    pub fn mask() -> u64 {
        (1 << SIZE as u64) - 1
    }

    pub fn mask_in_place() -> u64 {
        Self::mask() << POSITION as u64
    }

    pub fn shift() -> usize {
        POSITION
    }

    pub fn bitsize() -> usize {
        SIZE
    }
    pub fn is_valid(value: u64) -> bool {
        Self::decode(Self::encode_unchecked(value)) == value
    }

    pub fn decode(value: u64) -> u64 {
        if SIGN_EXTEND {
            ((value << (64 - Self::NEXT_BIT as u64)) as i64 >> (64 - SIZE as u64) as i64) as _
        } else {
            (value >> POSITION as u64) & Self::mask()
        }
    }
    pub fn encode(value: u64) -> u64 {
        debug_assert!(Self::is_valid(value));
        (value & Self::mask()) << POSITION as u64
    }

    pub fn update(value: u64, original: u64) -> u64 {
        Self::encode(value) | (!Self::mask_in_place() & original)
    }

    fn encode_unchecked(value: u64) -> u64 {
        (value & Self::mask()) << POSITION as u64
    }
}

pub struct AtomicBitField<'a, const SIZE: usize, const POSITION: usize, const SIGN_EXTEND: bool> {
    pub bits: &'a AtomicU64,
}

impl<'a, const SIZE: usize, const POSITION: usize, const SIGN_EXTEND: bool>
    AtomicBitField<'a, SIZE, POSITION, SIGN_EXTEND>
{
    pub fn new(bits: &'a AtomicU64) -> Self {
        Self { bits }
    }
    pub fn update_bool(&self, value: bool, order: Ordering) {
        if value {
            self.bits
                .fetch_or(BitField::<SIZE, POSITION, SIGN_EXTEND>::encode(1), order);
        } else {
            self.bits
                .fetch_and(!BitField::<SIZE, POSITION, SIGN_EXTEND>::encode(1), order);
        }
    }

    pub fn compare_exchange_weak(&self, old_tags: u64, new_tags: u64, order: Ordering) -> bool {
        self.bits
            .compare_exchange_weak(old_tags, new_tags, order, Ordering::Relaxed)
            .is_ok()
    }

    pub fn load(&self, order: Ordering) -> u64 {
        self.bits.load(order)
    }

    pub fn store(&self, value: u64, order: Ordering) {
        self.bits.store(value, order);
    }

    pub fn read(&self, order: Ordering) -> u64 {
        BitField::<SIZE, POSITION, SIGN_EXTEND>::decode(self.bits.load(order))
    }

    pub fn fetch_or(&self, value: u64, order: Ordering) -> u64 {
        self.bits.fetch_or(
            BitField::<SIZE, POSITION, SIGN_EXTEND>::encode(value),
            order,
        )
    }

    pub fn update(&self, value: u64) {
        let mut old_field = self.bits.load(Ordering::Relaxed);
        let mut new_field;
        loop {
            new_field = BitField::<SIZE, POSITION, SIGN_EXTEND>::update(value, old_field);
            match self.bits.compare_exchange_weak(
                old_field,
                new_field,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => old_field = x,
            }
        }
    }

    pub fn update_unsynchronized(&self, value: u64) {
        let old_field = self.bits.load(Ordering::Relaxed);
        let new_field;
        new_field = BitField::<SIZE, POSITION, SIGN_EXTEND>::update(value, old_field);
        self.bits.store(new_field, Ordering::Relaxed);
    }

    pub fn update_conditional(&self, value_to_be_set: u64, conditional_old_value: u64) -> u64 {
        let mut old_field = self.bits.load(Ordering::Relaxed);
        loop {
            let old_value = BitField::<SIZE, POSITION, SIGN_EXTEND>::decode(old_field);
            if old_value != conditional_old_value {
                return old_value;
            }

            let new_tags =
                BitField::<SIZE, POSITION, SIGN_EXTEND>::update(value_to_be_set, old_field);

            match self.bits.compare_exchange_weak(
                old_field,
                new_tags,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break value_to_be_set,
                Err(x) => old_field = x,
            }
        }
    }

    pub fn try_acquire(&self) -> bool {
        let mask = BitField::<SIZE, POSITION, SIGN_EXTEND>::encode(1);
        let old_field = self.bits.fetch_or(mask, Ordering::Relaxed);
        BitField::<SIZE, POSITION, SIGN_EXTEND>::decode(old_field) == 0
    }

    pub fn try_clear(&self) -> bool {
        let mask = BitField::<SIZE, POSITION, SIGN_EXTEND>::encode(1);
        let old_field = self.bits.fetch_and(!mask, Ordering::Relaxed);
        BitField::<SIZE, POSITION, SIGN_EXTEND>::decode(old_field) != 0
    }
}

pub trait ToAtomicBitField<'a, const SIZE: usize, const POSITION: usize, const SIGN_EXTEND: bool> {
    type Atomic;

    fn make_atomic(value: &'a u64) -> Self::Atomic;
}

impl<'a, const SIZE: usize, const POSITION: usize, const SIGN_EXTEND: bool>
    ToAtomicBitField<'a, SIZE, POSITION, SIGN_EXTEND> for BitField<SIZE, POSITION, SIGN_EXTEND>
{
    type Atomic = AtomicBitField<'a, SIZE, POSITION, SIGN_EXTEND>;

    fn make_atomic(value: &'a u64) -> Self::Atomic {
        Self::Atomic::new(unsafe { std::mem::transmute(value) })
    }
}