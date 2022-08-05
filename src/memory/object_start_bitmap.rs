use std::mem::size_of;

use crate::base::{
    constants::{ALLOCATION_GRANULARITY, ALLOCATION_MASK},
    utils::count_leading_zeros_8,
};

use super::{object_header::ObjectHeader, page::PAGE_SIZE};

pub const BITS_PER_CELL: usize = size_of::<u8>() * 8;
pub const CELL_MASK: usize = BITS_PER_CELL - 1;
pub const BITMAP_SIZE: usize = (PAGE_SIZE + ((BITS_PER_CELL * ALLOCATION_GRANULARITY) - 1))
    / (BITS_PER_CELL * ALLOCATION_GRANULARITY);
pub const RESERVED_FOR_BITMAP: usize = (BITMAP_SIZE + ALLOCATION_MASK) & !ALLOCATION_MASK;

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

    pub fn find_header(&self, maybe_middle_of_an_object: usize) -> *mut ObjectHeader {
        let object_offset = maybe_middle_of_an_object.wrapping_sub(self.offset);
        let object_start_number = object_offset.wrapping_div(ALLOCATION_GRANULARITY);
        let mut cell_index = object_start_number.wrapping_div(BITS_PER_CELL);

        let bit = object_start_number & CELL_MASK;
        let mut byte = self.object_start_bitmap[cell_index] & ((1 << (bit + 1)) - 1);

        while byte == 0 && cell_index > 0 {
            cell_index -= 1;
            byte = self.object_start_bitmap[cell_index];
        }

        let leading_zeros = count_leading_zeros_8(byte) as usize;

        let object_start_number = (cell_index.wrapping_mul(BITS_PER_CELL))
            .wrapping_add(BITS_PER_CELL - 1)
            .wrapping_sub(leading_zeros as usize);

        let object_offset = object_start_number.wrapping_mul(ALLOCATION_GRANULARITY);
        object_offset.wrapping_add(self.offset) as *mut ObjectHeader
    }
    pub fn set_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        self.object_start_bitmap[index] |= 1 << bit;
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
    pub fn object_start_index_bit(&self, addr: usize) -> (usize, usize) {
        let object_offset = addr.wrapping_sub(self.offset);
        let object_start_number = object_offset / ALLOCATION_GRANULARITY;

        (
            object_start_number / BITS_PER_CELL,
            object_start_number & CELL_MASK,
        )
    }
}
