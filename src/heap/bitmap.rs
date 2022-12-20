use std::{
    intrinsics::unlikely,
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use crate::{formatted_size, heap::{is_aligned, heap::heap}, utils::{sloppy_memset, round_up}};

pub struct HeapBitmap<const ALIGN: usize> {
    offset: usize,
    limit: usize,
    bmp: Box<[u8]>,
}

impl<const ALIGN: usize> HeapBitmap<ALIGN> {
    pub fn empty() -> Self {
        Self {
            limit: 0,
            offset: 0,
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
            limit: offset + arena_size,
            bmp: vec![0; Self::bitmap_size(arena_size)].into_boxed_slice(),
        }
    }

    pub const fn offset_index_bit(offset: usize) -> usize {
        (offset / ALIGN) % Self::bits_per_cell()
    }

    fn cell(&self, ix: usize) -> u8 {
        #[cfg(not(debug_assertions))]
        unsafe { *self.bmp.get_unchecked(ix) }
        #[cfg(debug_assertions)]
        {
            self.bmp[ix]
        }
    }

    pub fn find_object_start(&self, maybe_middle_of_an_object: usize) -> *mut u8 {
        let object_offset = maybe_middle_of_an_object.wrapping_sub(self.offset);
        let object_start_number = object_offset.wrapping_div(16);
        let mut cell_index = object_start_number.wrapping_div(Self::bits_per_cell());
        let bit = object_start_number & Self::cell_mask();
    
        let mut byte = self.cell(cell_index) as usize & ((1usize.wrapping_shl(bit as u32 + 1)) - 1);

        while byte == 0 && cell_index > 0 {
            cell_index -= 1;
            byte = self.cell(cell_index) as usize;
        }

        if byte == 0 {
            return null_mut();
        }
        let byte = byte as u8;
        let object_start_number = (cell_index.wrapping_mul(Self::bits_per_cell()))
            .wrapping_add(Self::bits_per_cell() - 1)
            .wrapping_sub(byte.leading_zeros() as usize);

        let object_offset = object_start_number.wrapping_mul(ALIGN);
        let addr = object_offset.wrapping_add(self.offset);
        assert!(addr >= self.offset && addr < self.limit, "addr is out of bounds: {:x}", addr);
        assert!(is_aligned(addr, ALIGN), "not aligned: {:x}", addr);
        addr as _
    }

    pub fn object_offset(&self, index: usize) -> usize {
        index * ALIGN * Self::bits_per_cell()
    }

    #[inline]
    pub fn set_bit(&self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        /*#[cfg(not(debug_assertions))]
        unsafe { *self.bmp.get_unchecked_mut(index) |= 1 << bit };
        #[cfg(debug_assertions)]
        {
            self.bmp[index] |= 1 << bit;
        }*/
        self.cell_atomic(index).fetch_or(1 << bit, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn cell_atomic(&self, index: usize) -> &AtomicU8 {
        #[cfg(not(debug_assertions))]
        unsafe { &*(self.bmp.as_ptr().add(index) as *const AtomicU8) }
        #[cfg(debug_assertions)]
        unsafe {
            std::mem::transmute(&self.bmp[index])
        }
    }

    #[inline]
    pub fn set_atomic(&self, addr: usize) {
        debug_assert!(heap().is_in(addr as _));
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index).load(Ordering::Acquire);

        self.cell_atomic(index)
            .store(cell | (1 << bit), Ordering::Release);
    }

    #[inline]
    pub fn clear_atomic(&self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index).load(Ordering::Acquire);

        self.cell_atomic(index)
            .store(cell & !(1 << bit), Ordering::Release);
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
        debug_assert!(is_aligned(addr, ALIGN));
        let (index, bit) = self.object_start_index_bit(addr);
        #[cfg(not(debug_assertions))]
        unsafe { (*self.bmp.get_unchecked(index) & (1 << bit)) != 0 }
        #[cfg(debug_assertions)]
        {
            self.bmp[index] & (1 << bit) != 0
        }
    }

    #[inline(always)]
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

        let start_index = begin_offset / ALIGN / Self::bits_per_cell();
        let end_index = end_offset / ALIGN / Self::bits_per_cell();

        self.bmp[start_index..end_index].fill(0);
    }

    pub fn is_marked_range(&self, begin: usize, end: usize) -> bool {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && Self::offset_index_bit(begin_offset) != 0 {
            debug_assert!(is_aligned(self.offset + begin_offset, ALIGN));
            debug_assert!(is_aligned(self.offset + end_offset, ALIGN));
            if self.check_bit(self.offset + begin_offset) {
                return true;
            }
            begin_offset += ALIGN;
        }

        while begin_offset < end_offset && Self::offset_index_bit(end_offset) != 0 {
            end_offset -= ALIGN;
            debug_assert!(is_aligned(self.offset + begin_offset, ALIGN));
            debug_assert!(is_aligned(self.offset + end_offset, ALIGN));
            if self.check_bit(self.offset + end_offset) {
                return true;
            }
        }

        let start_index = begin_offset / ALIGN / Self::bits_per_cell();
        let end_index = end_offset / ALIGN / Self::bits_per_cell();
        self.bmp[start_index..end_index].iter().any(|x| *x != 0)
    }

    pub fn clear_range_atomic(&self, begin: usize, end: usize) {
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

        let start_index = begin_offset / ALIGN / Self::bits_per_cell();
        let end_index = end_offset / ALIGN / Self::bits_per_cell();

        for index in start_index..end_index {
            self.cell_atomic(index).store(0, Ordering::Release);
        }
    }

    pub fn clear(&mut self) {
        unsafe {
            sloppy_memset::sloppy_memset(self.bmp.as_mut_ptr().cast(), 0, self.bmp.len());
        }
        //self.bmp.fill(0);
    }

    #[inline]
    pub fn fetch_and_set(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index);

        let old_val = cell.fetch_or(1 << bit, Ordering::AcqRel);

        old_val & (1 << bit) != 0
    }

    pub fn fetch_and_clear(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index);

        let old_val = cell.fetch_and(!(1 << bit), Ordering::AcqRel);

        old_val & (1 << bit) != 0
    }

    pub fn atomic_test_and_set(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        let cell = self.cell_atomic(index);

        let mut old_val = cell.load(Ordering::Relaxed);

        loop {
            let new_val = old_val | (1 << bit);

            if new_val == old_val {
                return false;
            }

            match cell.compare_exchange_weak(old_val, new_val, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(val) => old_val = val,
            }
        }
    }

    pub fn sweep_walk(
        live_bitmap: &Self,
        mark_bitmap: &Self,
        sweep_begin: usize,
        sweep_end: usize,
        mut cl: impl FnMut(&[usize]),
    ) {
        let buffer_size = size_of::<usize>() * Self::bits_per_cell();

        let live = live_bitmap.bmp.as_ptr() as *const AtomicU8;
        let mark = mark_bitmap.bmp.as_ptr() as *const AtomicU8;
        unsafe {
            let (start, _) = live_bitmap.object_start_index_bit(sweep_begin);
            let (end, _) = live_bitmap.object_start_index_bit(sweep_end);

            let (mark_start, _) = mark_bitmap.object_start_index_bit(sweep_begin);
            let (mark_end, _) = mark_bitmap.object_start_index_bit(sweep_end);

            let mut pointer_buf = vec![0usize; buffer_size];
            let mut cur_pointer = &mut pointer_buf[0] as *mut usize;
            let pointer_end = cur_pointer.add(buffer_size - 8);
            assert_eq!(mark_end - mark_start, end - start);
            for (il, im) in (start..=end).zip(mark_start..=mark_end) {
                let mut garbage = (*live.add(il)).load(Ordering::Relaxed)
                    & !(*mark.add(im)).load(Ordering::Relaxed);

                if unlikely(garbage != 0) {
                    let ptr_base = live_bitmap.object_offset(il);
                    loop {
                        let shift = garbage.trailing_zeros() as usize;
                        garbage ^= 1 << shift as usize;
                        cur_pointer.write(ptr_base + shift * ALIGN);
                        cur_pointer = cur_pointer.add(1);
                        if garbage == 0 {
                            break;
                        }
                    }

                    if cur_pointer >= pointer_end {
                        
                        cl(std::slice::from_raw_parts(
                            &pointer_buf[0],
                            (cur_pointer as usize - pointer_buf.as_ptr() as usize)
                                / size_of::<usize>(),
                        ));

                        cur_pointer = pointer_buf.as_mut_ptr();
                    }
                }
            }

            if cur_pointer > pointer_buf.as_mut_ptr() {
                cl(std::slice::from_raw_parts(
                    &pointer_buf[0],
                    (cur_pointer as usize - pointer_buf.as_ptr() as usize)
                        / size_of::<usize>(),
                ));
            }
        }
    }

    pub fn visit_marked_range(
        &self,
        visit_begin: usize,
        visit_end: usize,
        cl: impl Fn(*mut u8),
    ) {
        let (index_start, start_bit) = self.object_start_index_bit(visit_begin);
        let (index_end, end_bit) = self.object_start_index_bit(visit_end);

        let mut left_edge = self.bmp[index_start];

        left_edge &= !((1 << start_bit) - 1);

        let mut right_edge;

        if index_start < index_end {
            if left_edge != 0 {
                let ptr_base = self.object_offset(index_start) + self.offset;

                loop {
                    let shift = left_edge.trailing_zeros() as usize;
                    let obj = ptr_base + shift * ALIGN;
                    debug_assert!(heap().is_in(obj as _), "object 0x{:x} is not in heap (ptr_base={:x}, shift={:x}, left_edge={:x})", obj, ptr_base, shift, left_edge);
                    debug_assert!(is_aligned(obj, ALIGN) && self.check_bit(obj), "object 0x{:x} is not aligned (ptr_base={:x}, shift={:x}, left_edge={:x})", obj, ptr_base, shift, left_edge);
                    cl(obj as _);
                    left_edge ^= 1 << shift;

                    if left_edge == 0 {
                        break;
                    }
                }
            }

            for i in index_start + 1..index_end {
                let mut w = self.cell_atomic(i).load(Ordering::Relaxed);

                if w != 0 {
                    let ptr_base = self.object_offset(i) + self.offset;

                    loop {
                        let shift = w.trailing_zeros() as usize;
                        let obj = ptr_base + shift * ALIGN;
                        debug_assert!(heap().is_in(obj as _), "object 0x{:x} is not in heap (ptr_base={:x}, shift={:x}, i={:x}, w={:x})", obj, ptr_base, shift, i, w);
                        debug_assert!(is_aligned(obj, ALIGN) && self.check_bit(obj), "object 0x{:x} is not aligned (ptr_base={:x}, shift={:x}, i={:x}, w={:x})", obj, ptr_base, shift, i, w);
                        cl(obj as _);
                        w ^= 1 << shift;

                        if w == 0 {
                            break;
                        }
                    }
                }
            }

            if end_bit == 0 {
                right_edge = 0;
            } else {
                right_edge = self.bmp[index_end];
            }
        } else {
            right_edge = left_edge;
        }

        right_edge &= (1 << end_bit) - 1;

        if right_edge != 0 {
            let ptr_base = self.object_offset(index_end) + self.offset;

            loop {
                let shift = right_edge.trailing_zeros() as usize;
                let obj = ptr_base + shift * ALIGN;
                debug_assert!(heap().is_in(obj as _), "object 0x{:x} is not in heap (ptr_base={:x}, shift={:x}, right_edge={:x})", obj, ptr_base, shift, right_edge);
                debug_assert!(is_aligned(obj, ALIGN) && self.check_bit(obj), "object 0x{:x} is not aligned (ptr_base={:x}, shift={:x}, right_edge={:x})", obj, ptr_base, shift, right_edge);
                cl(obj as _);
                right_edge ^= 1 << shift;

                if right_edge == 0 {
                    break;
                }
            }
        }
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
        (heap_size + ((Self::bits_per_cell() * align) - 1)) / (Self::bits_per_cell() * align)
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
        unsafe { *self.bitmap.get_unchecked(ix) }
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

    pub fn new(heap: usize, heap_size: usize) -> Self {
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
