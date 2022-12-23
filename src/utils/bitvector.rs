use std::mem::size_of;

use super::sloppy_memset::sloppy_memset;

/// This is a space-efficient, resizeable bitvector class. In the common case it
/// occupies one word, but if necessary, it will inflate this one word to point
/// to a single chunk of out-of-line allocated storage to store an arbitrary number
/// of bits.
///
/// - The bitvector remembers the bound of how many bits can be stored, but this
///   may be slightly greater (by as much as some platform-specific constant)
///   than the last argument passed to ensureSize().
///
/// - The bitvector can resize itself automatically (set, clear, get) or can be used
///   in a manual mode, which is faster (quickSet, quickClear, quickGet, ensureSize).
///
/// - Accesses ASSERT that you are within bounds.
///
/// - Bits are automatically initialized to zero.
///
/// On the other hand, this BitVector class may not be the fastest around, since
/// it does conditionals on every get/set/clear. But it is great if you need to
/// juggle a lot of variable-length BitVectors and you're worried about wasting
/// space.
pub struct BitVector {
    bits_or_pointer: usize,
}

impl BitVector {
    pub const fn bits_in_pointer() -> usize {
        size_of::<usize>() << 3
    }

    pub const fn max_inline_bits() -> usize {
        BitVector::bits_in_pointer() - 1
    }

    pub const fn byte_count(bit_count: usize) -> usize {
        (bit_count + 7) >> 3
    }

    #[inline]
    fn make_inline_bits(bits: usize) -> usize {
        bits | (1 << Self::max_inline_bits())
    }

    #[inline]
    fn cleanse_inline_bits(bits: usize) -> usize {
        bits & !(1 << Self::max_inline_bits())
    }

    #[inline]
    const fn _bit_count(bits: usize) -> usize {
        bits.count_ones() as _
    }

    pub const fn is_inline(&self) -> bool {
        (self.bits_or_pointer >> Self::max_inline_bits()) != 0
    }

    fn out_of_line_bits(&self) -> &mut OutOfLineBits {
        unsafe { &mut *((self.bits_or_pointer << 1) as *mut OutOfLineBits) }
    }

    fn bits(&self) -> *const usize {
        if self.is_inline() {
            &self.bits_or_pointer
        } else {
            self.out_of_line_bits().bits()
        }
    }

    fn bits_mut(&mut self) -> *mut usize {
        if self.is_inline() {
            &mut self.bits_or_pointer
        } else {
            self.out_of_line_bits().bits()
        }
    }

    pub fn new() -> Self {
        Self {
            bits_or_pointer: Self::make_inline_bits(0),
        }
    }

    pub fn size(&self) -> usize {
        if self.is_inline() {
            Self::max_inline_bits()
        } else {
            self.out_of_line_bits().num_bits()
        }
    }

    fn set_slow(&mut self, other: &mut Self) {
        let new_bits_or_pointer;
        if other.is_inline() || self.is_empty_or_deleted_value() {
            new_bits_or_pointer = self.bits_or_pointer;
        } else {
            unsafe {
                let new_out_of_line_bits = OutOfLineBits::new(other.size());
                core::ptr::copy_nonoverlapping(
                    other.bits().cast::<u8>(),
                    (*new_out_of_line_bits).bits().cast::<u8>(),
                    Self::byte_count(other.size()),
                );
                new_bits_or_pointer = (new_out_of_line_bits as *mut OutOfLineBits) as usize >> 1;
            }
        }

        if !self.is_inline() || !self.is_empty_or_deleted_value() {
            unsafe {
                OutOfLineBits::destroy(self.out_of_line_bits());
            }
        }

        self.bits_or_pointer = new_bits_or_pointer;
    }

    fn resize_out_of_line(&mut self, num_bits: usize, shift_in_words: usize) {
        unsafe {
            let new_out_of_line_bits = OutOfLineBits::new(num_bits);
            let new_num_words = (*new_out_of_line_bits).num_words();

            if self.is_inline() {
                sloppy_memset(
                    (*new_out_of_line_bits).bits().cast(),
                    0,
                    shift_in_words * size_of::<usize>(),
                );
                *((*new_out_of_line_bits).bits().add(shift_in_words)) =
                    self.bits_or_pointer & !(1 << Self::max_inline_bits());
                sloppy_memset(
                    (*new_out_of_line_bits)
                        .bits()
                        .add(shift_in_words + 1)
                        .cast(),
                    0,
                    (new_num_words - shift_in_words - 1) * size_of::<usize>(),
                );
            } else {
                if num_bits > self.size() {
                    let old_num_words = self.out_of_line_bits().num_words();
                    sloppy_memset(
                        (*new_out_of_line_bits).bits().cast(),
                        0,
                        shift_in_words * size_of::<usize>(),
                    );
                    core::ptr::copy_nonoverlapping(
                        self.out_of_line_bits().bits(),
                        (*new_out_of_line_bits).bits().add(shift_in_words),
                        old_num_words,
                    );
                    sloppy_memset(
                        (*new_out_of_line_bits)
                            .bits()
                            .add(shift_in_words + old_num_words).cast(),
                        0,
                        (new_num_words - old_num_words - shift_in_words) * size_of::<usize>(),
                    );
                } else {
                    core::ptr::copy_nonoverlapping(self.out_of_line_bits().bits(), (*new_out_of_line_bits).bits(), (*new_out_of_line_bits).num_words());
                }

                
            }   
            self.bits_or_pointer = (new_out_of_line_bits as usize) >> 1;
        }   
    }

    fn merge_slow(&mut self, other: &Self) {
        if other.is_inline() {
            unsafe {
                *self.bits_mut() |= Self::cleanse_inline_bits(other.bits_or_pointer);
            }
            return;
        }   

        self.ensure_size(other.size());


        let a = self.out_of_line_bits();
        let b = other.out_of_line_bits();
        unsafe {
            for i in (0..b.num_words()).rev() {
                *a.bits().add(i) |= *b.bits().add(i);
            }
        }
    }

    fn filter_slow(&mut self, other: &Self) {
        if other.is_inline() {
            unsafe {
                *self.bits_mut() &= Self::cleanse_inline_bits(other.bits_or_pointer);
                return;
            }
        }

        if self.is_inline() {
            self.bits_or_pointer &= unsafe { other.out_of_line_bits().bits().read() };
            self.bits_or_pointer |= 1 << Self::max_inline_bits();
            return;
        }

        let a = self.out_of_line_bits();
        let b = other.out_of_line_bits();

        unsafe {
            for i in (0..a.num_words().min(b.num_words())).rev() {
                *a.bits().add(i) &= *b.bits().add(i);
            }

            for i in b.num_words()..a.num_words() {
                *a.bits().add(i) = 0;
            }
        }
    }

    fn exclude_slow(&mut self, other: &Self) {
        if other.is_inline() {
            unsafe {
                *self.bits_mut() &= !Self::cleanse_inline_bits(other.bits_or_pointer);
                return;
            }
        }

        if self.is_inline() {
            self.bits_or_pointer &= !unsafe { other.out_of_line_bits().bits().read() };
            self.bits_or_pointer |= 1 << Self::max_inline_bits();
            return;
        }

        let a = self.out_of_line_bits();
        let b = other.out_of_line_bits();

        unsafe {
            for i in (0..a.num_words().min(b.num_words())).rev() {
                *a.bits().add(i) &= !*b.bits().add(i);
            }
        }
    }

    fn bitcount_slow(&self) -> usize {
        let out_of_line_bits = self.out_of_line_bits();
        let mut result = 0;

        unsafe {
            for i in 0..out_of_line_bits.num_words() {
                result += (*out_of_line_bits.bits().add(i)).count_ones() as usize;
            }
        }

        result
    }

    fn is_empty_slow(&self) -> bool {
        let out_of_line_bits = self.out_of_line_bits();

        unsafe {
            for i in 0..out_of_line_bits.num_words() {
                if (*out_of_line_bits.bits().add(i)) != 0 {
                    return false;
                }
            }
        }

        true
    }

    fn equals_slow(&self, other: &Self) -> bool {
        for i in (0..self.size().max(other.size())).rev() {
            if self.get(i) != other.get(i) {
                return false;
            }
        }

        true
    }

    fn hash_slow_case(&self) -> usize {
        let mut result = 0;
        unsafe {
            let bits = self.out_of_line_bits();
            for i in (0..bits.num_words()).rev() {
                result ^= (*bits.bits().add(i)) as usize;
            }

            result
        }
    }

    pub fn ensure_size(&mut self, num_bits: usize) {
        if num_bits <= self.size() {
            return;
        }

        self.resize_out_of_line(num_bits, 0);
    }

    pub fn get(&self, bit: usize) -> bool {
        if bit >= self.size() {
            return false;
        }

        self.quick_get(bit)
    }

    pub fn quick_set(&mut self, bit: usize) -> bool {
        assert!(bit < self.size());
        unsafe {
            let at = self.bits_mut().add(bit / Self::bits_in_pointer());
            let word = at.read();
            let mask = 1 << (bit & (Self::bits_in_pointer() - 1));
            let result = (word & mask) != 0;
            at.write(word | mask);
            result
        }
    }

    pub fn quick_clear(&mut self, bit: usize) -> bool {
        assert!(bit < self.size());
        unsafe {
            let at = self.bits_mut().add(bit / Self::bits_in_pointer());
            let word = at.read();
            let mask = 1 << (bit & (Self::bits_in_pointer() - 1));
            let result = (word & mask) != 0;
            at.write(word & !mask);
            result
        }
    }

    pub fn set(&mut self, bit: usize) -> bool {
        self.ensure_size(bit + 1);
        self.quick_set(bit)
    }

    pub fn add(&mut self, bit: usize) -> bool {
        !self.set(bit)
    }

    pub fn ensure_size_and_set(&mut self, bit: usize, size: usize) -> bool {
        self.ensure_size(size);
        self.quick_set(bit)
    }

    pub fn clear(&mut self, bit: usize) -> bool {
        if bit >= self.size() {
            return false;
        }
        self.quick_clear(bit)
    }

    pub fn clear_all(&mut self) {
        if self.is_inline() {
            self.bits_or_pointer = Self::make_inline_bits(0);
        } else {
            unsafe {
                sloppy_memset(self.bits_mut().cast(), 0, Self::byte_count(self.size()));
            }
        }
    }

    pub fn merge(&mut self, other: &Self) {
        if !self.is_inline() || !other.is_inline() {
            return self.merge_slow(other);
        }

        self.bits_or_pointer |= other.bits_or_pointer;
    }

    pub fn filter(&mut self, other: &Self) {
        if !self.is_inline() || !other.is_inline() {
            return self.filter_slow(other);
        }

        self.bits_or_pointer &= other.bits_or_pointer;
    }

    pub fn exclude(&mut self, other: &Self) {
        if !self.is_inline() || !other.is_inline() {
            return self.exclude_slow(other);
        }

        self.bits_or_pointer &= !other.bits_or_pointer;
        self.bits_or_pointer |= 1 << Self::max_inline_bits();
    }

    pub fn bit_count(&self) -> usize {
        if self.is_inline() {
            return Self::cleanse_inline_bits(self.bits_or_pointer).count_ones() as usize;
        }

        self.bitcount_slow()
    }

    pub fn is_empty(&self) -> bool {
        if self.is_inline() {
            return Self::cleanse_inline_bits(self.bits_or_pointer) == 0;
        }

        self.is_empty_slow()
    }

    pub fn find_bit_simple(&self, mut index: usize, value :bool) -> usize {
        while index < self.size() {
            if self.get(index) == value {
                return index;
            }
            index += 1;
        }

        self.size()
    }

    pub fn find_bit(&self, index: usize, value :bool) -> usize {
        self.find_bit_simple(index, value)
    }



    pub fn quick_get(&self, bit: usize) -> bool {
        assert!(bit < self.size());
        unsafe {
            let at = self.bits().add(bit / Self::bits_in_pointer());
            let word = at.read();
            (word & (1 << (bit & (Self::bits_in_pointer() - 1)))) != 0
        }
    }

    const fn is_empty_or_deleted_value(&self) -> bool {
        self.bits_or_pointer <= 1
    }

    pub fn iter(&self) -> BitIter {
        BitIter {
            bvec: self,
            index: 0
        }
    }
}

#[repr(C)]
struct OutOfLineBits {
    num_bits: usize,
}

impl OutOfLineBits {
    unsafe fn new(num_bits: usize) -> *mut Self {
        let num_bits =
            (num_bits + BitVector::bits_in_pointer() - 1) & !(BitVector::bits_in_pointer() - 1);
        let size = size_of::<OutOfLineBits>()
            + size_of::<usize>() * (num_bits / BitVector::bits_in_pointer());

        let result = libc::malloc(size).cast::<Self>();
        result.write(Self { num_bits });

        result
    }

    unsafe fn destroy(this: *mut Self) {
        libc::free(this.cast())
    }

    fn num_bits(&self) -> usize {
        self.num_bits
    }

    fn num_words(&self) -> usize {
        (self.num_bits + BitVector::bits_in_pointer() - 1) / BitVector::bits_in_pointer()
    }

    fn bits(&self) -> *mut usize {
        unsafe { (self as *const Self).add(1) as *mut usize }
    }
}

impl Drop for BitVector {
    fn drop(&mut self) {
        if !self.is_inline() {
            unsafe {
                OutOfLineBits::destroy(self.out_of_line_bits());
            }
        }
    }
}

pub struct BitIter<'a> {
    bvec: &'a BitVector,
    index: usize 
}

impl Iterator for BitIter<'_> {
    type Item = usize;
    fn next(&mut self) -> Option<Self::Item> {
        let index = self.bvec.find_bit(self.index + 1, true);
        self.index = index;
        if index < self.bvec.size() {
            Some(index)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BitVector;

    #[test]
    fn test_bitvector() {
        let mut bvec = BitVector::new();

        bvec.set(0);
        bvec.set(35);
        assert!(bvec.get(0));
        assert!(!bvec.get(5));
        assert!(bvec.get(35));
        bvec.set(124);
        assert!(bvec.get(124));
        assert!(bvec.get(0));
        assert!(!bvec.get(5));
        assert!(bvec.get(35));
    }
}