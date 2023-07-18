use std::mem::size_of;

use crate::meta::block::BlockMeta;

pub const WORD_SIZE_BITS: usize = 3;
pub const WORD_SIZE: usize = 1 << WORD_SIZE_BITS;

pub const ALLOCATION_ALIGNMENT_WORDS: usize = 2;
pub const ALLOCATION_ALIGNMENT: usize = ALLOCATION_ALIGNMENT_WORDS * WORD_SIZE;
pub const ALLOCATION_ALIGNMENT_INVERSE_MASK: usize = !(ALLOCATION_ALIGNMENT - 1);

pub const BLOCK_SIZE_BITS: usize = 15;
pub const LINE_SIZE_BITS: usize = 8;
pub const BLOCK_COUNT_BITS: usize = 24;

pub const LINE_METADATA_SIZE_BITS: usize = 0;

pub const BLOCK_TOTAL_SIZE: usize = 1 << BLOCK_SIZE_BITS;
pub const LINE_SIZE: usize = 1 << LINE_SIZE_BITS;
pub const LINE_METADATA_SIZE: usize = 1 << LINE_METADATA_SIZE_BITS;
pub const MAX_BLOCK_COUNT: usize = 1 << BLOCK_COUNT_BITS;

pub const LINE_COUNT: usize = BLOCK_TOTAL_SIZE / LINE_SIZE;

pub const WORDS_IN_LINE: usize = LINE_SIZE / WORD_SIZE;
pub const WORDS_IN_BLOCK: usize = BLOCK_TOTAL_SIZE / WORD_SIZE;

pub const BLOCK_SIZE_IN_BYTES_MASK: usize = BLOCK_TOTAL_SIZE - 1;
pub const BLOCK_SIZE_IN_BYTES_MASK_INVERSE_MASK: usize = !BLOCK_SIZE_IN_BYTES_MASK;

pub const LARGE_BLOCK_SIZE_BITS: usize = 13;
pub const LARGE_BLOCK_SIZE: usize = 1 << LARGE_BLOCK_SIZE_BITS;

pub const LARGE_OBJECT_MIN_SIZE_BITS: usize = 13;

pub const MIN_BLOCK_SIZE: usize = 1 << LARGE_OBJECT_MIN_SIZE_BITS;

pub const LARGE_BLOCK_MASK: usize = !((1 << LARGE_BLOCK_SIZE_BITS) - 1);

pub const EARLY_GROW_THRESHOLD: usize = 128 * 1024 * 1024;
pub const EARLY_GROWTH_RATE: f64 = 2.0;
pub const GROWTH_RATE: f64 = 1.4142135623730951;

pub const METADATA_PER_BLOCK: usize = size_of::<BlockMeta>() + LINE_COUNT * LINE_METADATA_SIZE + WORDS_IN_BLOCK / ALLOCATION_ALIGNMENT_WORDS;
pub const SPACE_USED_PER_BLOCK: usize = BLOCK_TOTAL_SIZE + METADATA_PER_BLOCK;
pub const MAX_HEAP_SIZE: usize = SPACE_USED_PER_BLOCK * MAX_BLOCK_COUNT;
pub const MIN_HEAP_SIZE: usize = 1 * 1024 * 1024;
pub const UNLIMITED_HEAP_SIZE: usize = !0;

