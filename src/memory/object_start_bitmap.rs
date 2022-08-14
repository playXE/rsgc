use std::{mem::size_of, ptr::null_mut};

use crate::base::constants::{ALLOCATION_GRANULARITY, ALLOCATION_MASK};

use super::{object_header::ObjectHeader, page::PAGE_SIZE};

pub const BITS_PER_CELL: usize = size_of::<u8>() * 8;
pub const CELL_MASK: usize = BITS_PER_CELL - 1;
pub const BITMAP_SIZE: usize = (PAGE_SIZE + ((BITS_PER_CELL * ALLOCATION_GRANULARITY) - 1))
    / (BITS_PER_CELL * ALLOCATION_GRANULARITY);
pub const RESERVED_FOR_BITMAP: usize = (BITMAP_SIZE + ALLOCATION_MASK) & !ALLOCATION_MASK;

#[repr(C)]
pub struct ObjectStartBitmap {
    offset: usize,
    fully_populated: bool,
    object_start_bitmap: [u8; RESERVED_FOR_BITMAP],
}

impl ObjectStartBitmap {
    pub fn mark_as_fully_populated(&mut self) {
        self.fully_populated = true;
    }
    pub fn new(offset: usize) -> Self {
        let mut this = Self {
            offset,
            fully_populated: false,
            object_start_bitmap: [0; RESERVED_FOR_BITMAP],
        };

        this.clear();
        this.fully_populated = true;

        this
    }

    pub fn offset_index_bit(offset: usize) -> usize {
        (offset / ALLOCATION_GRANULARITY) % BITS_PER_CELL
    }
    pub fn clear_range(&mut self, begin: usize, end: usize) {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && Self::offset_index_bit(begin_offset) != 0 {
            self.clear_bit(self.offset + begin_offset);
            begin_offset += ALLOCATION_GRANULARITY;
        }

        while begin_offset < end_offset && Self::offset_index_bit(end_offset) != 0 {
            end_offset -= ALLOCATION_GRANULARITY;
            self.clear_bit(self.offset + end_offset);
        }
    }
    pub fn find_header(&self, maybe_middle_of_an_object: usize) -> *mut ObjectHeader {
        let object_offset = maybe_middle_of_an_object.wrapping_sub(self.offset);
        let object_start_number = object_offset.wrapping_div(ALLOCATION_GRANULARITY);
        let mut cell_index = object_start_number.wrapping_div(BITS_PER_CELL);

        let bit = object_start_number & CELL_MASK;
        let mut byte = self.object_start_bitmap[cell_index] & ((1 << (bit + 1)) - 1) as u8;

        while byte == 0 && cell_index > 0 {
            cell_index -= 1;
            byte = self.object_start_bitmap[cell_index];
        }

        let object_start_number = (cell_index.wrapping_mul(BITS_PER_CELL))
            .wrapping_add(BITS_PER_CELL - 1)
            .wrapping_sub(byte.leading_zeros() as usize);

        let object_offset = object_start_number.wrapping_mul(ALLOCATION_GRANULARITY);
        let addr = object_offset.wrapping_add(self.offset);
        if addr < self.offset {
            // if overflow happens this might point to `page->object_start() - 16` instead and that is not valid location.
            return null_mut();
        } else {
            addr as _
        }
    }
    #[inline]
    pub fn set_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        unsafe { *self.object_start_bitmap.get_unchecked_mut(index) |= 1 << bit };
    }

    pub fn clear_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        self.object_start_bitmap[index] &= !(1 << bit);
    }

    pub fn check_bit(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        (self.object_start_bitmap[index] & (1 << bit)) != 0
    }
    pub fn clear(&mut self) {
        self.fully_populated = false;
        self.object_start_bitmap.fill(0);
    }
    #[inline]
    pub fn object_start_index_bit(&self, addr: usize) -> (usize, usize) {
        let object_offset = addr.wrapping_sub(self.offset);
        let object_start_number = object_offset / ALLOCATION_GRANULARITY;

        (
            object_start_number / BITS_PER_CELL,
            object_start_number & CELL_MASK,
        )
    }
}
