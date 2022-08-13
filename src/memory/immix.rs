use std::mem::size_of;

use super::object_start_bitmap::ObjectStartBitmap;

pub const IMMIX_CHUNK_SIZE: usize = 4 * 1024 * 1024;
pub const IMMIX_CHUNK_SIZE_LOG2: usize = 22;
pub const IMMIX_BLOCK_SIZE: usize = 32 * 1024;
pub const IMMIX_LINE_SIZE: usize = 256;
pub const LINES_PER_BLOCK: usize = IMMIX_BLOCK_SIZE / IMMIX_LINE_SIZE;
pub const LINES_PER_CHUNK: usize = BLOCKS_PER_CHUNK * LINES_PER_BLOCK;
pub const BLOCKS_PER_CHUNK: usize = (IMMIX_CHUNK_SIZE - size_of::<ImmixChunk>()) / IMMIX_BLOCK_SIZE;

pub struct ImmixChunk {
    object_start_bitmap: ObjectStartBitmap,
}