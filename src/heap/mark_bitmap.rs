use std::{
    mem::size_of,
    ops::{Index, IndexMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use super::{memory_region::MemoryRegion, align_up, virtual_memory};

pub type Idx = usize;
pub type BmWord = usize;

pub struct MarkBitmap {
    shift: isize,
    covered: MemoryRegion,
    map: *mut BmWord,
    size: Idx,
}

impl MarkBitmap {
    pub const FIND_ONES_FLIP: usize = 0;
    pub const FIND_ZEROS_FLIP: usize = !0;
    pub const BITS_PER_WORD: usize = size_of::<usize>() * 8;

    #[cfg(target_pointer_width = "64")]
    pub const LOG_BITS_PER_WORD: usize = 3;
    #[cfg(target_pointer_width = "32")]
    pub const LOG_BITS_PER_WORD: usize = 2;

    // Threshold for performing small range operation, even when large range
    // operation was requested. Measured in words.
    pub const SMALL_RANGE_WORDS: usize = 32;

    pub const fn bit_mask(bit: Idx) -> BmWord {
        1 << Self::bit_in_word(bit)
    }

    pub const fn bit_index(bit: Idx) -> Idx {
        bit >> Self::LOG_BITS_PER_WORD
    }

    pub const fn bit_in_word(bit: Idx) -> Idx {
        bit & (Self::BITS_PER_WORD - 1)
    }

    pub fn map(&self) -> *mut BmWord {
        self.map
    }

    pub const fn to_words_align_down(bit: Idx) -> Idx {
        bit >> Self::LOG_BITS_PER_WORD
    }

    pub const fn to_words_align_up(bit: Idx) -> Idx {
        Self::to_words_align_down(bit + (Self::BITS_PER_WORD - 1))
    }

    pub fn word_addr(&self, bit: Idx) -> *mut BmWord {
        unsafe { self.map().add(Self::to_words_align_down(bit)) }
    }

    pub fn word_addr_atomic(&self, bit: Idx) -> &AtomicUsize {
        unsafe { std::mem::transmute(self.map().add(Self::to_words_align_down(bit))) }
    }

    pub fn at(&self, index: Idx) -> bool {
        let word = unsafe { *self.word_addr(index) };
        word & Self::bit_mask(index) != 0
    }

    pub fn address_to_index(&self, addr: *mut u8) -> usize {
        unsafe { (((addr as *mut usize).sub_ptr(self.covered.start() as *const usize) << 1) >> self.shift) as _ }
    }

    pub fn index_to_address(&self, offset: usize) -> *mut u8 {
        (self.covered.start() + ((offset >> 1) << self.shift as usize) * size_of::<usize>()) as _
    }

    pub fn mark_strong(&self, heap_addr: *mut u8, was_upgraded: &mut bool) -> bool {
        let bit = self.address_to_index(heap_addr);
        let addr = self.word_addr_atomic(bit);
        let mask = Self::bit_mask(bit);
        let mask_weak = 1 << (Self::bit_in_word(bit) + 1);
        let mut old_val = addr.load(Ordering::Relaxed);

        loop {
            let new_val = old_val | mask;

            if new_val == old_val {
                assert!(*was_upgraded, "should be false already");
                return false;
            }

            match addr.compare_exchange_weak(old_val, new_val, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(cur_val) => {
                    *was_upgraded = (cur_val & mask_weak) != 0;
                    return true;
                }
                Err(cur_val) => old_val = cur_val,
            }
        }
    }

    pub fn mark_weak(&self, heap_addr: *mut u8) -> bool {
        let bit = self.address_to_index(heap_addr);
        let addr = self.word_addr_atomic(bit);
        let mask_strong = 1 << Self::bit_in_word(bit);
        let mask_weak = 1 << (Self::bit_in_word(bit) + 1);

        let mut old_val = addr.load(Ordering::Relaxed);

        loop {
            if (old_val & mask_strong) != 0 {
                return false;
            }
            let new_val = old_val | mask_weak;

            if new_val == old_val {
                return false;
            }

            match addr.compare_exchange_weak(old_val, new_val, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => {
                    return true;
                }
                Err(cur_val) => old_val = cur_val,
            }
        }
    }

    pub fn is_marked_strong(&self, addr: *mut u8) -> bool {
        self.at(self.address_to_index(addr))
    }

    pub fn is_marked_weak(&self, addr: *mut u8) -> bool {
        self.at(self.address_to_index(addr) + 1)
    }

    pub fn is_marked(&self, addr: *mut u8) -> bool {
        let index = self.address_to_index(addr);
        let mask = 3 << Self::bit_in_word(index);
        unsafe { (self.word_addr(index).read() & mask) != 0 }
    }

    pub fn clear_range_of_words(&self, beg: usize, end: usize) {
        for i in beg..end {
            unsafe {
                self.map.add(i).write(0);
            }
        }
    }

    pub fn clear_large_range_of_words(&self, beg: usize, end: usize) {
        unsafe {
            assert!(beg <= end, "underflow");
            libc::memset(self.map.add(beg) as _, 0, (end - beg) * size_of::<usize>());
        }
    }

    pub const fn inverted_bit_mask_for_range(beg: usize, end: usize) -> usize {
        let mask = Self::bit_mask(beg) - 1;
        if Self::bit_in_word(end) != 0 {
            mask | !(Self::bit_mask(end) - 1)
        } else {
            mask
        }
    }

    pub fn clear_range_within_word(&self, beg: usize, end: usize) {
        if beg != end {
            let mask = Self::inverted_bit_mask_for_range(beg, end);
            unsafe {
                *self.word_addr(beg) &= mask;
            }
        }
    }

    pub fn clear_range(&self, beg: usize, end: usize) 
    {
        self.verify_range(beg, end);
        let beg_full_word = Self::to_words_align_up(beg);
        let end_full_word = Self::to_words_align_up(end);

        if beg_full_word < end_full_word {
            self.clear_range_within_word(beg, Self::bit_index(beg_full_word));
            self.clear_range_of_words(beg_full_word, end_full_word);
            self.clear_range_within_word(Self::bit_index(end_full_word), end);
        } else {
            let boundary = Self::bit_index(beg_full_word).min(end);
            self.clear_range_within_word(beg, boundary);
            self.clear_range_within_word(boundary, end);
        }
    }

    pub fn is_small_range_of_words(beg_full_word: usize, end_full_word: usize) -> bool {
        beg_full_word + Self::SMALL_RANGE_WORDS >= end_full_word
    }

    pub fn clear_large_range(&self, beg: usize, end: usize) {
        self.verify_range(beg, end);
        let beg_full_word = Self::to_words_align_up(beg);
        let end_full_word = Self::to_words_align_up(end);

        if Self::is_small_range_of_words(beg_full_word, end_full_word) {
            self.clear_range(beg, end);
            return;
        }

        self.clear_range_within_word(beg, Self::bit_index(beg_full_word));
        self.clear_large_range_of_words(beg_full_word, end_full_word);
        self.clear_range_within_word(Self::bit_index(end_full_word), end);
    }

    pub fn clear_large_range_mr(&self, mr: MemoryRegion) {
        let beg = self.address_to_index(mr.start() as _);
        let end = self.address_to_index(mr.end() as _);
        self.clear_large_range(beg, end);
    }

    pub const fn mark_distance() -> usize {
        (16 * 8) / 2
    }

    pub fn compute_size(heap_size: usize) -> usize {
        align_up(heap_size / Self::mark_distance(), virtual_memory::page_size())
    }

    pub fn new(heap: MemoryRegion, storage: MemoryRegion, word_size: usize) -> Self {
        Self {
            shift: 4,
            covered: heap,
            map: storage.start() as _,
            size: (word_size * 2) >> 4
        }
    }

    fn verify_index(&self, bit: usize) {
        debug_assert!(bit < self.size, "BitMap index out of bounds: {} >= {}", bit, self.size);
    }

    fn verify_limit(&self, bit: Idx) {
        debug_assert!(bit <= self.size, "BitMap limit out of bounds: {} >= {}", bit, self.size);
    }

    fn verify_range(&self, beg: usize, end: usize) {
        debug_assert!(beg <= end, "BitMap range error: {} > {}", beg, end);
        self.verify_limit(end);
    }

}

impl Index<usize> for MarkBitmap {
    type Output = BmWord;
    fn index(&self, index: usize) -> &Self::Output {
        unsafe { &*self.map().add(index) }
    }
}

impl IndexMut<usize> for MarkBitmap {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        unsafe { &mut *self.map().add(index) }
    }
}
