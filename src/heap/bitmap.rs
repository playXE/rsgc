use std::{mem::size_of, ptr::null_mut, sync::atomic::{AtomicU8, Ordering}};


pub struct HeapBitmap<const ALIGN: usize> {
    offset: usize,
    bmp: Box<[u8]>,
}

impl<const ALIGN: usize> HeapBitmap<ALIGN> {
    pub fn empty() -> Self {
        Self {
            offset: 0,
            bmp: Box::new([])
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
        (arena_size + ((Self::bits_per_cell() * ALIGN) - 1))
            / (Self::bits_per_cell() * ALIGN)
    }

    pub const fn reserved_for_bitmap(arena_size: usize) -> usize {
        (Self::bitmap_size(arena_size) + Self::ALIGN_MASK) & !Self::ALIGN_MASK
    }

    pub fn new(offset: usize, arena_size: usize) -> Self {
        Self {
            offset,
            bmp: vec![0; Self::bitmap_size(arena_size)].into_boxed_slice(),
        }
    }

    pub const fn offset_index_bit(offset: usize) -> usize {
        (offset / ALIGN) % Self::bits_per_cell()
    }

    fn cell(&self, ix: usize) -> u8 {
        unsafe {
            *self.bmp.get_unchecked(ix)
        }
    }

    pub fn find_object_start(&self, maybe_middle_of_an_object: usize) -> *mut u8 {
        let object_offset = maybe_middle_of_an_object.wrapping_sub(self.offset);
        let object_start_number = object_offset.wrapping_div(16);
        let mut cell_index = object_start_number.wrapping_div(Self::bits_per_cell());
        let bit = object_start_number & Self::cell_mask();
        let mut byte = self.cell(cell_index) & ((1 << (bit + 1)) - 1) as u8;

        while byte == 0 && cell_index > 0 {
            cell_index -= 1;
            byte = self.cell(cell_index);
        }

        if byte == 0 {
            return null_mut();
        }

        let object_start_number = (cell_index.wrapping_mul(Self::bits_per_cell()))
            .wrapping_add(Self::bits_per_cell() - 1)
            .wrapping_sub(byte.leading_zeros() as usize);

        let object_offset = object_start_number.wrapping_mul(16);
        let addr = object_offset.wrapping_add(self.offset);
        debug_assert!(addr >= self.offset);
        addr as _
    }

    #[inline]
    pub fn set_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        unsafe { *self.bmp.get_unchecked_mut(index) |= 1 << bit };
    }

    pub fn cell_atomic(&self, index: usize) -> &AtomicU8 {
        unsafe {
            &*(self.bmp.as_ptr().add(index) as *const AtomicU8)
        }
    }

    #[inline]
    pub fn set_atomic(&self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index).load(Ordering::Acquire);

        self.cell_atomic(index).store(cell | (1 << bit), Ordering::Release);
    }

    #[inline]
    pub fn clear_atomic(&self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index).load(Ordering::Acquire);

        self.cell_atomic(index).store(cell & !(1 << bit), Ordering::Release);
    }

    #[inline]
    pub fn check_atomic(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index).load(Ordering::Acquire);

        cell & (1 << bit) != 0
    }

    #[inline]
    pub fn clear_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        self.bmp[index] &= !(1 << bit);
    }

    #[inline]
    pub fn check_bit(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        (self.bmp[index] & (1 << bit)) != 0
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

    pub fn clear_range(&mut self, begin: usize, end: usize) {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && Self::offset_index_bit(begin_offset) != 0 {
            self.clear_bit(self.offset + begin_offset);
            begin_offset += ALIGN;
        }

        while begin_offset < end_offset && Self::offset_index_bit(end_offset) != 0 {
            end_offset -= ALIGN;
            self.clear_bit(self.offset + end_offset);
        }
    }

    pub fn clear_range_atomic(&mut self, begin: usize, end: usize) {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && Self::offset_index_bit(begin_offset) != 0 {
            self.clear_atomic(self.offset + begin_offset);
            begin_offset += ALIGN;
        }

        while begin_offset < end_offset && Self::offset_index_bit(end_offset) != 0 {
            end_offset -= ALIGN;
            self.clear_atomic(self.offset + end_offset);
        }
    }

    pub fn clear(&mut self) {
        self.bmp.fill(0);
    }
}


/// Bitmap that allows to mark objects with arbitrary alignment in a heap. 
pub struct CHeapBitmap {
    align: usize,
    offset: usize,
    bitmap: Box<[u8]>,
}

impl CHeapBitmap {
    #[inline]
    pub const fn bits_per_cell() -> usize {
        8 * size_of::<u8>()
    }

    #[inline]
    pub const fn cell_mask() -> usize {
        Self::bits_per_cell() - 1
    }
    
    #[inline]
    pub const fn bitmap_size(heap_size: usize, align: usize) -> usize {
        (heap_size + ((Self::bits_per_cell() * align) - 1))
            / (Self::bits_per_cell() * align)
    }   

    #[inline]
    pub const fn reserved_for_bitmap(heap_size: usize, align: usize) -> usize {
        (Self::bitmap_size(heap_size, align) + align - 1) & !(align - 1)
    }

    pub fn empty() -> Self {
        Self {
            align: 0,
            offset: 0,
            bitmap: vec![].into_boxed_slice(),
        }
    }

    pub fn new(align: usize, offset: usize, heap_size: usize) -> Self {
        
        Self {
            align,
            offset,
            bitmap: vec![0; Self::bitmap_size(heap_size, align)].into_boxed_slice(),
        }
    }

    fn cell(&self, ix: usize) -> u8 {
        unsafe {
            *self.bitmap.get_unchecked(ix)
        }
    }

    #[inline]
    pub fn set_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        unsafe { *self.bitmap.get_unchecked_mut(index) |= 1 << bit };
    }

    #[inline]
    pub fn check_bit(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        (self.bitmap[index] & (1 << bit)) != 0
    }

    #[inline]
    pub fn clear_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        self.bitmap[index] &= !(1 << bit);
    }
    #[inline]
    pub const fn offset_index_bit(&self, offset: usize) -> usize {
        (offset / self.align) % Self::bits_per_cell()
    }

    pub fn clear_range(&mut self, begin: usize, end: usize) {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && self.offset_index_bit(begin_offset) != 0 {
            self.clear_bit(self.offset + begin_offset);
            begin_offset += self.align;
        }

        while begin_offset < end_offset && self.offset_index_bit(end_offset) != 0 {
            end_offset -= self.align;
            self.clear_bit(self.offset + end_offset);
        }
    }

    pub fn clear(&mut self) {
        self.bitmap.fill(0);
    }


    #[inline]
    pub fn object_start_index_bit(&self, addr: usize) -> (usize, usize) {
        let object_offset = addr.wrapping_sub(self.offset);
        let object_start_number = object_offset / self.align;

        (
            object_start_number / Self::bits_per_cell(),
            object_start_number & Self::cell_mask(),
        )
    }

    #[inline]
    pub fn count_ones(&self) -> usize {
        self.bitmap.iter().map(|x| x.count_ones() as usize).sum()
    }
}

pub struct HeapBytemap<const ALIGN: usize> {
    offset: usize,
    bytes: Box<[u8]>,
}

impl<const ALIGN: usize> HeapBytemap<ALIGN> {
    pub const ALIGN_MASK: usize = ALIGN - 1;
    
    pub const fn bytemap_size(heap_size: usize) -> usize {
        (heap_size + ALIGN - 1) / ALIGN
    }

    pub const fn reserved_for_bytemap(heap_size: usize) -> usize {
        (Self::bytemap_size(heap_size) + Self::ALIGN_MASK) & !Self::ALIGN_MASK
    }

    pub fn new(heap: usize,heap_size: usize) -> Self {
        Self {
            offset: heap,
            bytes: vec![0; Self::bytemap_size(heap_size)].into_boxed_slice(),
        }
    }

    #[inline]
    pub fn set_byte(&mut self, addr: usize, byte: u8) {
        let offset = self.offset - addr;
        let index = offset / ALIGN;
        unsafe { *self.bytes.get_unchecked_mut(index) = byte };
    }

    #[inline]
    pub fn get_byte(&self, addr: usize) -> u8 {
        let offset = self.offset - addr;
        let index = offset / ALIGN;
        unsafe { *self.bytes.get_unchecked(index) }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.bytes.fill(0);
    }

    pub const fn offset_index_byte(offset: usize) -> usize {
        offset / ALIGN
    }

    pub fn clear_range(&mut self, begin: usize, end: usize) {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && Self::offset_index_byte(begin_offset) != 0 {
            self.set_byte(self.offset + begin_offset, 0);
            begin_offset += ALIGN;
        }

        while begin_offset < end_offset && Self::offset_index_byte(end_offset) != 0 {
            end_offset -= ALIGN;
            self.set_byte(self.offset + end_offset, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cheap_bitmap() {
        let heap = vec![0u8; 4 * 1024 * 1024].into_boxed_slice();
        let heap_ptr = heap.as_ptr() as usize;

        let mut bmp1 = super::CHeapBitmap::new(256, heap_ptr, heap.len());

        bmp1.set_bit(heap_ptr + 24);
        bmp1.set_bit(heap_ptr + 512);
        assert!(bmp1.check_bit(heap_ptr));
        assert!(bmp1.check_bit(heap_ptr + 512));
        assert!(!bmp1.check_bit(heap_ptr + 256));

    }
}