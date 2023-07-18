use crate::meta::block::{get_block_index, get_from_index, BlockMeta};

#[derive(Clone, Copy)]
pub struct BlockList {
    block_meta_start: *mut usize,
    first: *mut BlockMeta,
    last: *mut BlockMeta,
}

impl BlockList {
    pub unsafe fn get_next_block(
        block_meta_start: *mut usize,
        block_meta: *mut BlockMeta,
    ) -> *mut BlockMeta {
        let mut next_block_id = (*block_meta).next_block;

        if next_block_id == -1 {
            return core::ptr::null_mut();
        } else if next_block_id == 0 {
            next_block_id = get_block_index(block_meta_start, block_meta) as i32 + 1;
        }

        get_from_index(block_meta_start, next_block_id as _)
    }

    pub fn new(block_meta_start: *mut usize) -> Self {
        Self {
            block_meta_start,
            first: core::ptr::null_mut(),
            last: core::ptr::null_mut(),
        }
    }

    pub fn poll(&mut self) -> *mut BlockMeta {
        let block = self.first;

        if !block.is_null() {
            unsafe {
                if block == self.last {
                    self.first = core::ptr::null_mut();
                }   

                self.first = Self::get_next_block(self.block_meta_start, block);

            }
        }

        block
    }

    pub fn add_last(&mut self, block_meta: *mut BlockMeta) {
        if self.first.is_null() {
            self.first = block_meta;
        } else {
            unsafe {
                (*self.last).next_block = get_block_index(self.block_meta_start, block_meta) as _;
            }
        }
        
        self.last = block_meta;
        unsafe {
            (*block_meta).next_block = -1;
        }
    }

    pub fn add_block_last(&mut self, first: *mut BlockMeta, last: *mut BlockMeta) {
        if self.first.is_null() {
            self.first = first;
        } else {
            unsafe {
                (*self.last).next_block = get_block_index(self.block_meta_start, first) as _;
            }
        }

        self.last = last;
        unsafe {
            (*last).next_block = -1;
        }
    }

    pub fn clear(&mut self) {
        self.first = core::ptr::null_mut();
        self.last = core::ptr::null_mut();
    }
}
