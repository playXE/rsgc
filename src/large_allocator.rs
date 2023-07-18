use std::ptr::null_mut;

use crate::{
    block_allocator::BlockAllocator,
    constants::{
        ALLOCATION_ALIGNMENT, BLOCK_SIZE_BITS, BLOCK_TOTAL_SIZE, LARGE_OBJECT_MIN_SIZE_BITS,
        MIN_BLOCK_SIZE, WORDS_IN_BLOCK, WORD_SIZE,
    },
    meta::{
        block::{get_block_start, BlockFlag, BlockMeta},
        object::{sweep, ObjectMeta},
    },
    object_header::ObjectHeader,
    utils::{
        bytemap::Bytemap,
        math::{div_and_round_up, round_to_next_multiple},
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Chunk {
    pub nothing: *mut (),
    pub size: usize,
    pub next: *mut Chunk,
}

pub const FREE_LIST_COUNT: usize = (1 << (BLOCK_SIZE_BITS - LARGE_OBJECT_MIN_SIZE_BITS)) - 1;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FreeList {
    pub first: *mut Chunk,
    pub last: *mut Chunk,
}

impl FreeList {
    pub unsafe fn add_last(&mut self, chunk: *mut Chunk) {
        if self.first.is_null() {
            self.first = chunk;
        }

        self.last = chunk;
        (*chunk).next = core::ptr::null_mut();
    }

    pub fn remove_first(&mut self) -> *mut Chunk {
        if self.first.is_null() {
            return core::ptr::null_mut();
        }

        let chunk = self.first;

        if self.first == self.last {
            self.last = core::ptr::null_mut();
        }

        self.first = unsafe { (*chunk).next };
        chunk
    }

    pub const fn new() -> Self {
        Self {
            first: core::ptr::null_mut(),
            last: core::ptr::null_mut(),
        }
    }
}

pub struct LargeAllocator {
    pub freelist: [FreeList; FREE_LIST_COUNT],
    pub heap_start: *mut usize,
    pub block_meta_start: *mut usize,
    pub bytemap: *mut Bytemap,
    pub block_allocator: *mut BlockAllocator,
}

impl LargeAllocator {
    pub const fn size_to_linked_list_index(size: usize) -> usize {
        size / MIN_BLOCK_SIZE - 1
    }

    pub unsafe fn chunk_and_offset(chunk: *mut Chunk, offset: isize) -> *mut Chunk {
        chunk.cast::<u8>().offset(offset).cast()
    }

    pub fn new(
        block_allocator: *mut BlockAllocator,
        bytemap: *mut Bytemap,
        block_meta_start: *mut usize,
        heap_start: *mut usize,
    ) -> Self {
        Self {
            heap_start,
            block_allocator,
            bytemap,
            block_meta_start,
            freelist: [FreeList::new(); FREE_LIST_COUNT],
        }
    }

    pub fn add_chunk(&mut self, chunk: *mut Chunk, total_block_size: usize) {
        assert!(total_block_size >= MIN_BLOCK_SIZE);
        assert!(total_block_size < BLOCK_TOTAL_SIZE);
        assert!(total_block_size % MIN_BLOCK_SIZE == 0);

        let list_index = Self::size_to_linked_list_index(total_block_size);

        unsafe {
            (*chunk).nothing = null_mut();
            (*chunk).size = total_block_size;
            let chunk_meta = (*self.bytemap).get(chunk.cast());
            chunk_meta.write(ObjectMeta::Placeholder);

            self.freelist[list_index].add_last(chunk);
        }
    }

    fn get_chunk_for_size(&mut self, required_chunk_size: usize) -> *mut Chunk {
        let list_index = Self::size_to_linked_list_index(required_chunk_size);

        for list_index in list_index..FREE_LIST_COUNT {
            let chunk = self.freelist[list_index].first;
            if !chunk.is_null() {
                self.freelist[list_index].remove_first();

                return chunk;
            }
        }

        core::ptr::null_mut()
    }

    pub fn get_block(&mut self, requested_block_size: usize) -> *mut ObjectHeader {
       
        let actual_block_size = round_to_next_multiple(requested_block_size, MIN_BLOCK_SIZE);

        let mut chunk = null_mut();

        if actual_block_size < BLOCK_TOTAL_SIZE {
            chunk = self.get_chunk_for_size(actual_block_size);
        }

        if chunk.is_null() {
            let humongous_size = div_and_round_up(actual_block_size, BLOCK_TOTAL_SIZE);
            
            let humongous =
                unsafe { (*self.block_allocator).get_free_humongous_block(humongous_size) };

            if !humongous.is_null() {
                unsafe {
                    chunk =
                        get_block_start(self.block_meta_start, self.heap_start, humongous).cast();
                    (*chunk).nothing = null_mut();
                    (*chunk).size = humongous_size * BLOCK_TOTAL_SIZE;
                }
            }
        }

        if chunk.is_null() {
            return null_mut();
        }

        unsafe {
            let chunk_size = (*chunk).size;

            if chunk_size.wrapping_sub(MIN_BLOCK_SIZE) >= actual_block_size {
                let remaining_chunk = Self::chunk_and_offset(chunk, actual_block_size as _);

                let remaining_chunk_size = chunk_size - actual_block_size;
                self.add_chunk(remaining_chunk, remaining_chunk_size);
            }

            let object_meta = (*self.bytemap).get(chunk.cast());
            object_meta.write(ObjectMeta::Allocated);
            core::ptr::write_bytes(chunk.cast::<u8>(), 0, actual_block_size);

            chunk.cast()
        }
    }

    pub fn clear(&mut self) {
        for list in self.freelist.iter_mut() {
            list.first = core::ptr::null_mut();
            list.last = core::ptr::null_mut();
        }
    }

    pub fn sweep(&mut self, block_meta: *mut BlockMeta, block_start: *mut usize) {
        // Objects that are larger than a block
        // are always allocated at the begining the smallest possible humongous block.
        // Any gaps at the end can be filled with large objects, that are smaller
        // than a block. This means that objects can ONLY start at the begining at
        // the first block or anywhere at the last block, except the begining.
        // Therefore we only need to look at a few locations.
        unsafe {
            let humongous_size = (*block_meta).humongous_size() as usize;
            let block_end = block_start.add(humongous_size * WORDS_IN_BLOCK);

            let first_object = (*self.bytemap).get(block_start);
            assert_ne!(first_object.read(), ObjectMeta::Free);
            let last_block = block_meta.add(humongous_size - 1);

            if humongous_size > 1 && first_object.read() != ObjectMeta::Marked {
                (*self.block_allocator).add_free_blocks(block_meta, humongous_size - 1);
                (*last_block).set_flag(BlockFlag::HumongousStart);
                (*last_block).set_humongous_size(1);
            }

            let last_block_start = block_end.sub(WORDS_IN_BLOCK);
            let mut chunk_start = null_mut();

            if first_object.read() != ObjectMeta::Marked {
                chunk_start = last_block_start;
            }

            sweep(first_object);

            let mut current = last_block_start.add(MIN_BLOCK_SIZE / WORD_SIZE);
            let mut current_meta = (*self.bytemap).get(current);

            while current < block_end {
                if chunk_start.is_null() {
                    if matches!(
                        current_meta.read(),
                        ObjectMeta::Allocated | ObjectMeta::Placeholder
                    ) {
                        chunk_start = current;
                    }
                } else {
                    if current_meta.read() == ObjectMeta::Marked {
                        let current_size = (current.offset_from(chunk_start)) as usize * WORD_SIZE;

                        self.add_chunk(chunk_start.cast(), current_size);
                        chunk_start = null_mut();
                    }
                }

                sweep(current_meta);

                current = current.add(MIN_BLOCK_SIZE / WORD_SIZE);
                current_meta = current_meta.add(MIN_BLOCK_SIZE / ALLOCATION_ALIGNMENT);
            }

            if chunk_start == last_block_start {
                // free chunk covers the entire last block, released it to the block
                // allocator
                (*self.block_allocator).add_free_blocks(last_block, 1);
            } else {
                let current_size = (block_end.offset_from(chunk_start)) as usize * WORD_SIZE;
                self.add_chunk(chunk_start.cast(), current_size);
            }
        }
    }
}
