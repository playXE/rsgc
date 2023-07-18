use crate::{
    constants::{ALLOCATION_ALIGNMENT_INVERSE_MASK, ALLOCATION_ALIGNMENT_WORDS, WORDS_IN_LINE, ALLOCATION_ALIGNMENT},
    meta::object::ObjectMeta,
};

#[repr(C)]
pub struct Bytemap {
    pub(crate) first_address: *mut usize,
    pub size: usize,
    pub end: *mut ObjectMeta,
    pub data: [ObjectMeta; 0],
}

impl Bytemap {
    pub fn is_ptr_aligned(address: *const usize) -> bool {
        let aligned = address as usize & ALLOCATION_ALIGNMENT_INVERSE_MASK;
     
        aligned == address as usize
    }

    #[inline]
    pub unsafe fn index(&self, address: *const usize) -> usize {
        let index = address.offset_from(self.first_address) / ALLOCATION_ALIGNMENT_WORDS as isize;

        debug_assert!(address >= self.first_address, "{:p} {:p}", address, self.first_address);
        debug_assert!(index < self.size as isize, "index {} out of bounds {} for {:p}", index, self.size, address);

        index as usize
    }
    #[inline]
    pub unsafe fn get(&mut self, address: *const usize) -> *mut ObjectMeta {
        self.data.as_mut_ptr().add(self.index(address))
    }

    pub unsafe fn next_line(cursor: *mut ObjectMeta) -> *mut ObjectMeta {
        cursor.add(WORDS_IN_LINE / ALLOCATION_ALIGNMENT_WORDS)
    }

    pub unsafe fn init(this: *mut Bytemap, first_address: *mut usize, size: usize) {
        (*this).first_address = first_address;
        (*this).size = size / ALLOCATION_ALIGNMENT;
        (*this).end = (*this).data.as_mut_ptr().add((*this).size);
    }
}
