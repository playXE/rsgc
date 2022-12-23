use std::{mem::size_of, sync::atomic::AtomicUsize};

use atomic::Ordering;

use crate::utils::sloppy_memset::sloppy_memset;

use super::{align_up, memory_region::MemoryRegion};

pub struct MarkBitmap {
    map: *mut usize,
    size: usize,
    shift: isize,
    covered: MemoryRegion,
}

#[cfg(target_pointer_width = "64")]
pub const LOG_BITS_PER_WORD: usize = 3;
#[cfg(target_pointer_width = "32")]
pub const LOG_BITS_PER_WORD: usize = 2;

pub const BITS_PER_WORD: usize = size_of::<usize>() * 8;

pub const FIND_ONES_FLIP: usize = 0;
pub const FIND_ZEROS_FLIP: usize = !0;

pub const LOG_MIN_OBJECT_ALIGNMENT: usize = 4;

impl MarkBitmap {
    pub const SMALL_RANGE_WORDS: usize = 32;

    pub fn map(&self) -> *mut usize {
        self.map
    }

    pub const fn raw_to_words_align_down(bit: usize) -> usize {
        bit >> LOG_BITS_PER_WORD
    }

    pub const fn raw_to_words_align_up(bit: usize) -> usize {
        Self::raw_to_words_align_down(bit + (BITS_PER_WORD - 1))
    }

    pub fn to_words_align_up(&self, bit: usize) -> usize {
        self.verify_limit(bit);
        Self::raw_to_words_align_up(bit)
    }

    pub fn to_words_align_down(&self, bit: usize) -> usize {
        self.verify_limit(bit);
        Self::raw_to_words_align_down(bit)
    }

    pub const fn bit_mask(bit: usize) -> usize {
        1 << Self::bit_in_word(bit)
    }

    pub const fn bit_in_word(bit: usize) -> usize {
        bit & (BITS_PER_WORD - 1)
    }

    pub const fn bit_index(word: usize) -> usize {
        word << LOG_BITS_PER_WORD
    }

    pub fn word_addr(&self, bit: usize) -> *mut usize {
        unsafe { self.map.add(self.to_words_align_down(bit)) }
    }

    pub fn at(&self, index: usize) -> bool {
        unsafe {
            self.verify_index(index);
            (self.word_addr(index).read() & Self::bit_mask(index)) != 0
        }
    }

    pub fn map_at(&self, index: usize) -> usize {
        unsafe {
            self.verify_index(index);
            self.word_addr(index).read()
        }
    }

    fn verify_limit(&self, bit: usize) {
        assert!(
            bit <= self.size,
            "BitMap limit is out of bounds: {} > {}",
            bit,
            self.size
        )
    }

    fn verify_range(&self, start: usize, end: usize) {
        assert!(start <= end, "BitMap range is invalid: {} > {}", start, end);
        self.verify_limit(end);
    }

    fn verify_index(&self, index: usize) {
        assert!(
            index < self.size,
            "BitMap index is out of bounds: {} > {}",
            index,
            self.size
        )
    }


    pub fn new(heap: MemoryRegion, storage: MemoryRegion) -> Self {
        Self {
            shift: LOG_MIN_OBJECT_ALIGNMENT as _,
            covered: heap,
            map: storage.pointer().cast(),
            size: (heap.word_size() * 2) >> LOG_MIN_OBJECT_ALIGNMENT,
        }
    }

    pub const fn compute_size(heap_size: usize) -> usize {
        align_up(heap_size / Self::mark_distance(), 16)
    }

    pub const fn mark_distance() -> usize {
        16 * 8 / 2
    }

    pub fn address_to_index(&self, addr: *mut usize) -> isize {
        unsafe { (addr.offset_from(self.covered.pointer().cast()) << 1) >> self.shift }
    }

    pub fn index_to_address(&self, index: usize) -> *mut usize {
        (self.covered.start() + ((index >> 1) << self.shift)) as _
    }


    #[inline]
    pub fn mark_strong(&self, heap_addr: *mut usize, was_upgraded: &mut bool) -> bool {
        let bit = self.address_to_index(heap_addr) as usize;

        unsafe {
            let addr = &*self.word_addr(bit).cast::<AtomicUsize>();
            let mask = Self::bit_mask(bit);
            let mask_weak = 1 << (Self::bit_in_word(bit) + 1);
            let mut old_val = addr.load(Ordering::Relaxed);

            loop {
                let new_val = old_val | mask;
                if new_val == old_val {
                    return false;
                }

                match addr.compare_exchange_weak(old_val, new_val, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => {
                        *was_upgraded = (old_val & mask_weak) != 0;
                        return true;
                    }
                    Err(val) => old_val = val,
                }
            }
        }
    }

    #[inline]
    pub fn mark_weak(&self, heap_addr: *mut usize) -> bool {
        let bit = self.address_to_index(heap_addr) as usize;

        unsafe {
            let addr = &*self.word_addr(bit).cast::<AtomicUsize>();
            let mask = Self::bit_mask(bit) | (1 << (Self::bit_in_word(bit) + 1));
            let mask_strong = Self::bit_mask(bit);
            let mut old_val = addr.load(Ordering::Relaxed);

            loop {
                if (old_val & mask_strong) != 0 {
                    return false;
                }

                let new_val = old_val | mask;
                if new_val == old_val {
                    return false;
                }

                match addr.compare_exchange_weak(old_val, new_val, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => return true,
                    Err(val) => old_val = val,
                }
            }
        }
    }

    #[inline]
    pub fn is_marked_strong(&self, heap_addr: *mut usize) -> bool {
        self.at(self.address_to_index(heap_addr) as _)
    }

    #[inline]
    pub fn is_marked_weak(&self, heap_addr: *mut usize) -> bool {
        self.at(self.address_to_index(heap_addr) as usize + 1)
    }

    #[inline]
    pub fn is_marked(&self, heap_addr: *mut usize) -> bool {
        let index = self.address_to_index(heap_addr);
        let mask = 3 << Self::bit_in_word(index as _);
        unsafe {
            (self.word_addr(index as _).read() & mask) != 0
        }
    }

    #[inline]
    pub fn clear(&self) {
        unsafe {
            sloppy_memset(self.map.cast(), 0, self.size);
        }
    }   

    pub fn is_marked_strong_range(&self, begin: usize, end: usize) -> bool {
        let start_index = self.address_to_index(begin as _) as usize;
        let end_index = self.address_to_index(end as _) as usize;

        unsafe {
            let slice = std::slice::from_raw_parts(self.map.add(start_index), end_index - start_index);
            slice.iter().any(|&x| x != 0)
        }
    }
}
