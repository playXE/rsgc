use std::{
    mem::size_of,
    ops::{Index, IndexMut},
    sync::atomic::{AtomicUsize, Ordering, AtomicU8}, ptr::null_mut,
};
use crate::utils::*;
use super::{memory_region::MemoryRegion, align_up, virtual_memory};
pub struct MarkBitmap {
    offset: usize,
    mark_bit: bool,
    bmp: Box<[u8]>,
}

const ALIGN: usize = 16;

impl MarkBitmap {
    pub fn empty() -> Self {
        Self {
            offset: 0,
            mark_bit: true,
            bmp: Box::new([]),
        }
    }

    pub const ALIGN_MASK: usize = ALIGN - 1;

    pub const fn bits_per_cell() -> usize {
        8 * size_of::<u8>()
    }

    pub const fn cell_mask() -> usize {
        Self::bits_per_cell() - 1
    }

    pub const fn bitmap_size(arena_size: usize) -> usize {
        (arena_size + ((Self::bits_per_cell() * ALIGN) - 1)) / (Self::bits_per_cell() * ALIGN)
    }

    pub const fn reserved_for_bitmap(arena_size: usize) -> usize {
        (Self::bitmap_size(arena_size) + Self::ALIGN_MASK) & !Self::ALIGN_MASK
    }

    pub fn new(offset: usize, arena_size: usize) -> Self {
        Self {
            offset,
            mark_bit: true,
            bmp: vec![0; Self::bitmap_size(arena_size)].into_boxed_slice(),
        }
    }

    pub const fn offset_index_bit(offset: usize) -> usize {
        (offset / ALIGN) % Self::bits_per_cell()
    }

    fn cell(&self, ix: usize) -> u8 {
        unsafe { *self.bmp.get_unchecked(ix) }
    }

    #[inline]
    pub fn object_start_index_bit(&self, addr: usize) -> (usize, usize) {
        let object_offset = addr.wrapping_sub(self.offset);
        let object_start_number = object_offset / ALIGN;

        (
            object_start_number / Self::bits_per_cell(),
            object_start_number & Self::cell_mask(),
        )
    }
    
    pub fn cell_atomic(&self, ix: usize) -> &AtomicU8 {
        unsafe { &*(self.bmp.as_ptr().add(ix) as *const AtomicU8) }
    }

    pub fn atomic_test_and_set(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index);

        let mut old_val = cell.load(Ordering::Relaxed);

        let mask = 1 << bit;

        loop {
            let new_val = if self.mark_bit {
                old_val | mask
            } else {
                old_val & !mask
            };

            if new_val == old_val {
                return false;
            }
            
            match cell.compare_exchange_weak(old_val, new_val, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(v) => old_val = v
            }
        }
    }

    pub fn is_marked(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell(index);
        
        ((cell & (1 << bit)) != 0) == self.mark_bit
    }

    pub fn is_marked_range(&self, begin: usize, end: usize) -> bool {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && Self::offset_index_bit(begin_offset) != 0 {
            if self.is_marked(self.offset + begin_offset) {
                return true;
            }
            begin_offset += ALIGN;
        }

        while begin_offset < end_offset && Self::offset_index_bit(end_offset) != 0 {
            end_offset -= ALIGN;
            
            if self.is_marked(self.offset + end_offset) {
                return true;
            }
        }

        let start_index = begin_offset / ALIGN / Self::bits_per_cell();
        let end_index = end_offset / ALIGN / Self::bits_per_cell();
        self.bmp[start_index..end_index].iter().any(|x| (*x != 0) == self.mark_bit)
    }

    

    pub fn flip_mark_bit(&mut self ) {
        self.mark_bit = !self.mark_bit;
    }

    pub fn clear(&mut self) {
        self.bmp.fill(0);
    }
}