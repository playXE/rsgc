
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
    #[inline(always)]
    pub const fn mask() -> u64 {
        (1 << SIZE as u64) - 1
    }
    #[inline(always)]
    pub const fn mask_in_place() -> u64 {
        Self::mask() << POSITION as u64
    }
    #[inline(always)]
    pub const fn shift() -> usize {
        POSITION
    }
    #[inline(always)]
    pub const fn bitsize() -> usize {
        SIZE
    }
    #[inline(always)]
    pub const fn is_valid(value: u64) -> bool {
        Self::decode(Self::encode_unchecked(value)) == value
    }
    #[inline(always)]
    pub const fn decode(value: u64) -> u64 {
        if SIGN_EXTEND {
            ((value << (64 - Self::NEXT_BIT as u64)) as i64 >> (64 - SIZE as u64) as i64) as _
        } else {
            (value >> POSITION as u64) & Self::mask()
        }
    }   
    #[inline(always)]
    pub const fn encode(value: u64) -> u64 {
        (value & Self::mask()) << POSITION as u64
    }
    #[inline(always)]
    pub const fn update(value: u64, original: u64) -> u64 {
        Self::encode(value) | (!Self::mask_in_place() & original)
    }
    #[inline(always)]
    const fn encode_unchecked(value: u64) -> u64 {
        (value & Self::mask()) << POSITION as u64
    }
}