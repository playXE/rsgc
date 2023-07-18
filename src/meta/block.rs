use std::{mem::size_of, ptr::null_mut};

use crate::{
    constants::{
        BLOCK_SIZE_IN_BYTES_MASK_INVERSE_MASK, LINE_COUNT, LINE_SIZE_BITS, WORDS_IN_BLOCK,
        WORDS_IN_LINE,
    },
    utils::{u24::u24, bytemap::Bytemap}, block_allocator::BlockAllocator,
};

use super::{object::{clear_block_at, sweep_line_at, clear_line_at}, line::{LineFlag, FreeLineMeta}};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlockFlag {
    Free = 0x0,
    Simple = 0x1,
    HumongousStart = 0x2,
    HumongousContinuation = 0x3,
    Marked = 0x5,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub union BlockMetaU {
    pub simple: (u8, i8),
    pub humongous: (u8, u24),
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct BlockMeta {
    pub(crate) block: BlockMetaU,
    pub(crate) next_block: i32,
}

impl BlockMeta {
    pub fn is_free(&self) -> bool {
        unsafe { self.block.simple.0 == BlockFlag::Free as u8 }
    }

    pub fn is_simple_block(&self) -> bool {
        unsafe { (self.block.simple.0 & BlockFlag::Simple as u8) != 0 }
    }

    pub fn is_humongous_start(&self) -> bool {
        unsafe { self.block.simple.0 == BlockFlag::HumongousStart as u8 }
    }

    pub fn is_humongous_continuation(&self) -> bool {
        unsafe { self.block.simple.0 == BlockFlag::HumongousContinuation as u8 }
    }

    pub fn humongous_size(&self) -> u32 {
        unsafe { self.block.humongous.1.into() }
    }

    pub fn contains_large_objects(&self) -> bool {
        self.is_humongous_start() || self.is_humongous_continuation()
    }

    pub fn first_free_line(&self) -> i8 {
        unsafe { self.block.simple.1 }
    }

    pub fn set_humongous_size(&mut self, size: u32) {
        self.block.humongous.1 = u24::from(size);
    }

    pub fn set_first_free_line(&mut self, line: i8) {
        self.block.simple.1 = line;
    }

    pub fn set_flag(&mut self, flag: BlockFlag) {
        self.block.simple.0 = flag as u8;
    }

    pub fn is_marked(&self) -> bool {
        unsafe { self.block.simple.0 == BlockFlag::Marked as u8 }
    }

    pub fn unmark(&mut self) {
        self.block.simple.0 = BlockFlag::Simple as u8;
    }

    pub fn mark(&mut self) {
        self.block.simple.0 = BlockFlag::Marked as u8;
    }
}

pub fn get_line_address(block_start: *mut usize, line_index: isize) -> *mut usize {
    unsafe { block_start.offset(WORDS_IN_LINE as isize * line_index) }
}

pub fn get_block_end(block_start: *mut usize) -> *mut usize {
    unsafe { block_start.offset(WORDS_IN_LINE as isize * LINE_COUNT as isize) }
}

pub fn get_line_index_from_word(block_start: *mut usize, word: *const usize) -> usize {
    ((word as isize - block_start as isize) >> LINE_SIZE_BITS as isize) as _
}

pub fn get_line_word(block_start: *mut usize, line_index: isize, word_index: isize) -> *mut usize {
    unsafe { get_line_address(block_start, line_index).offset(word_index) }
}

pub unsafe fn get_humongous_start(
    block_meta_start: *mut usize,
    block_meta: *mut BlockMeta,
) -> *mut BlockMeta {
    let mut current = block_meta;

    while (*current).is_humongous_continuation() {
        current = current.sub(1);
        assert!(current.cast::<usize>() >= block_meta_start);
    }
    assert!((*current).is_humongous_start());
    current
}

pub fn get_block_index(block_meta_start: *mut usize, block_meta: *mut BlockMeta) -> usize {
    unsafe { block_meta.offset_from(block_meta_start.cast::<BlockMeta>()) as _ }
}

pub fn get_block_start_for_word(word: *const usize) -> *mut usize {
    (word as usize & BLOCK_SIZE_IN_BYTES_MASK_INVERSE_MASK) as _
}

pub fn get_block_index_for_word(heap_start: *mut usize, word: *const usize) -> isize {
    unsafe {
        let block_start = get_block_start_for_word(word);
        block_start.offset_from(heap_start) / WORDS_IN_BLOCK as isize
    }
}

pub fn get_block_start(
    block_meta_start: *mut usize,
    heap_start: *mut usize,
    block_meta: *mut BlockMeta,
) -> *mut usize {
    unsafe {
        let index = get_block_index(block_meta_start, block_meta);
        heap_start.offset(index as isize * WORDS_IN_BLOCK as isize)
    }
}
pub fn get_from_index(block_meta_start: *mut usize, index: usize) -> *mut BlockMeta {
    unsafe { block_meta_start.add(index) as _ }
}

pub fn get_block_meta(
    block_meta_start: *mut usize,
    heap_start: *mut usize,
    word: *const usize,
) -> *mut BlockMeta {
    let index = get_block_index_for_word(heap_start, word);
    get_from_index(block_meta_start, index as _)
}

pub unsafe fn recycle_unmarked_block(block_allocator: *mut BlockAllocator, bytemap: *mut Bytemap, block_meta: *mut BlockMeta, block_start: *mut usize) {
    core::ptr::write_bytes(block_meta.cast::<u8>(), 0, size_of::<BlockMeta>());

    (*block_allocator).add_free_blocks(block_meta, 1);
    clear_block_at((*bytemap).get(block_start));
}

pub unsafe fn recycle_block(block_allocator: *mut BlockAllocator, bytemap: *mut Bytemap, block_meta: *mut BlockMeta, block_start: *mut usize,line_metas: *mut u8) {
    if !(*block_meta).is_marked() {
        recycle_unmarked_block(block_allocator, bytemap, block_meta, block_start);
    } else {
        (*block_meta).unmark();

        // start at line zero, keep separate pointers into all affected data
        // structures
        let mut line_index = 0;
        let mut line_meta = line_metas;
        let mut line_start = block_start;

        let mut bytemap_cursor = (*bytemap).get(line_start);

        let mut last_recyclable = null_mut::<FreeLineMeta>();
    
        while line_index < LINE_COUNT {
        
            if line_meta.read() == LineFlag::Marked as u8 {
                line_meta.write(LineFlag::Empty as u8);
                sweep_line_at(bytemap_cursor);
                line_index += 1;
                line_meta = line_meta.add(1);
                line_start = line_start.add(WORDS_IN_LINE);
                bytemap_cursor = Bytemap::next_line(bytemap_cursor);
            } else {
                // If the line is not marked, we need to merge all continuous
                // unmarked lines.

                // If it's the first free line, update the block header to point
                // to it.

                if last_recyclable.is_null() {
                    (*block_meta).set_first_free_line(line_index as _);
                } else {
                    // Update the last recyclable line to point to the current
                    // one
                    (*last_recyclable).next = line_index as _;
                }

                clear_line_at(bytemap_cursor);
                last_recyclable = line_start.cast::<FreeLineMeta>();

                line_index += 1;
                line_meta = line_meta.add(1);
                line_start = line_start.add(WORDS_IN_LINE);
                bytemap_cursor = Bytemap::next_line(bytemap_cursor);

                let mut size = 1;

                while line_index < LINE_COUNT && line_meta.read() == LineFlag::Empty as u8 {
                    clear_line_at(bytemap_cursor);
                    size += 1;

                    line_index += 1;
                    line_meta = line_meta.add(1);
                    line_start = line_start.add(WORDS_IN_LINE);
                    bytemap_cursor = Bytemap::next_line(bytemap_cursor);
                }

                (*last_recyclable).size = size as _;
            }
        }

        if !last_recyclable.is_null() {
            (*last_recyclable).next = -1;
            (*block_allocator).recycle(block_meta);

        }
    }
}