use std::ptr::null_mut;

use crate::meta;
use crate::utils::block_list::BlockList;
use crate::utils::math::{log2floor, log2ceil};
use crate::{constants::BLOCK_COUNT_BITS, meta::block::BlockMeta};
use parking_lot::RawMutex as Lock;
use parking_lot::lock_api::RawMutex;
pub const SUPERBLOCK_LIST_SIZE: usize = BLOCK_COUNT_BITS - 1;

pub struct BlockAllocator {
    recycled_blocks: BlockList,
    pub recycled_blocks_count: u32,
    smallest_humongous_block: (*mut BlockMeta, *mut BlockMeta),
    min_non_empty_index: i32,
    max_non_empty_index: i32,
    pub free_block_count: u32,
    coalescing_humongous_block: (*mut BlockMeta, *mut BlockMeta),
    free_humongous_blocks: [BlockList; SUPERBLOCK_LIST_SIZE],
    allocation_lock: Lock,
}

impl BlockAllocator {
    pub fn new(block_meta_start: *mut usize, block_count: u32) -> Self {
        let mut this = Self {
            recycled_blocks: BlockList::new(block_meta_start),
            recycled_blocks_count: 0,
            smallest_humongous_block: (null_mut(), null_mut()),
            min_non_empty_index: 0,
            max_non_empty_index: 0,
            free_block_count: block_count,
            coalescing_humongous_block: (null_mut(), null_mut()),
            free_humongous_blocks: [BlockList::new(block_meta_start);SUPERBLOCK_LIST_SIZE],
            allocation_lock: Lock::INIT
        };


        this.clear();   
        unsafe {
            this.smallest_humongous_block = (block_meta_start.cast(), block_meta_start.cast::<BlockMeta>().add(block_count as usize))
        }
        this 
    }

    pub fn recycle(&mut self, block_meta: *mut BlockMeta) {
        
        self.recycled_blocks.add_last(block_meta);
        self.recycled_blocks_count += 1;
    }

    pub fn try_recycled(&mut self) -> *mut BlockMeta {
        if self.recycled_blocks_count == 0 {
            return null_mut();
        }

        self.recycled_blocks_count -= 1;
        self.acquire();
        let block = self.recycled_blocks.poll();
        self.release();
        block
    }

    pub fn clear(&mut self) {
        for i in 0..SUPERBLOCK_LIST_SIZE {
            self.free_humongous_blocks[i].clear();
        }

        self.free_block_count = 0;
        self.smallest_humongous_block = (null_mut(), null_mut());
        self.coalescing_humongous_block = (null_mut(), null_mut());
        self.min_non_empty_index = SUPERBLOCK_LIST_SIZE as i32;
        self.max_non_empty_index = -1;
    }

    pub fn acquire(&self) {
        self.allocation_lock.lock();
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
    }

    pub fn release(&self) {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        unsafe { self.allocation_lock.unlock(); }
    }

    pub fn size_to_linked_list_index(size: usize) -> isize {
        log2floor(size)
    }

    pub fn poll_humongous(&mut self, first: usize) -> *mut BlockMeta {
        let max_non_empty_index = self.max_non_empty_index;
        for i in first as i32..=max_non_empty_index {
            let block = self.free_humongous_blocks[i as usize].poll();

            if !block.is_null() {
                return block;
            } else {
                self.min_non_empty_index = i + 1;
            }
        }

        null_mut()
    }
    #[inline(never)]
    #[cold]
    pub fn get_free_block_slow(&mut self) -> *mut BlockMeta {
        let block = self.poll_humongous(self.min_non_empty_index as _);

        if !block.is_null() {
            unsafe {
                self.smallest_humongous_block.0 = block.add(1);
                self.smallest_humongous_block.1 = block.add((*block).humongous_size() as _);

                (*block).set_humongous_size(0);
                (*block).set_flag(meta::block::BlockFlag::Simple);

                return block;
            }
        }

        null_mut()
    }

    pub fn get_free_block(&mut self) -> *mut BlockMeta {
        let block;
        self.acquire();
        if self.smallest_humongous_block.0 >= self.smallest_humongous_block.1 {
            
            block = self.get_free_block_slow();
        } else {
            block = self.smallest_humongous_block.0;
            unsafe {
                self.smallest_humongous_block.0 = block.add(1);
                (*block).set_flag(meta::block::BlockFlag::Simple);
            }
        }
        self.release();
        block
    }

    pub fn get_free_humongous_block(&mut self, size: usize) -> *mut BlockMeta {
        self.acquire();
        let humongous;

        unsafe {
            let off = self.smallest_humongous_block.1.offset_from(self.smallest_humongous_block.0);

            if off >= size as isize {
                humongous = self.smallest_humongous_block.0;
                self.smallest_humongous_block.0 = self.smallest_humongous_block.0.add(size);
            } else {
                let target = log2ceil(size);
                let min_non_empty_index = self.min_non_empty_index as isize;
                let first = if min_non_empty_index > target {
                    min_non_empty_index as usize 
                } else {
                    target as usize
                };
                humongous = self.poll_humongous(first);

                if humongous.is_null() {
                    self.release();
                    return null_mut();
                }

                if (*humongous).humongous_size() > size as u32 {
                    let leftover = humongous.add(size);
                    self.add_free_blocks_internal(leftover, (*humongous).humongous_size() as usize - size);
                }
            }   

            self.release();

            (*humongous).set_humongous_size(size as u32);
            (*humongous).set_flag(meta::block::BlockFlag::HumongousStart);
            let limit = humongous.add(size);
            
            let mut current = humongous.add(1);

            while current < limit {
                (*current).set_flag(meta::block::BlockFlag::HumongousContinuation);
                current = current.add(1);
            }

            humongous
        }
    }

    unsafe fn add_free_blocks_internal0(&mut self, block: *mut BlockMeta, count: usize) {
        let i = Self::size_to_linked_list_index(count);

        if i < self.min_non_empty_index as isize {
            self.min_non_empty_index = i as _;
        }

        if i > self.max_non_empty_index as isize {
            self.max_non_empty_index = i as _;
        }

        let limit = block.add(count);
        let mut current = block;

        while current < limit {
            (*current).set_flag(meta::block::BlockFlag::Free);
            current = current.add(1);
        }

        (*block).set_humongous_size(count as _);
        self.free_humongous_blocks[i as usize].add_last(block);
    }   

    unsafe fn add_free_blocks_internal(&mut self, block: *mut BlockMeta, count: usize) {
        let mut remaining_count = count as u32;
        let mut power_of_two = 1u32;
        let mut current = block;

        while remaining_count > 0 {
            if (power_of_two & remaining_count) > 0 {
                self.add_free_blocks_internal0(current, power_of_two as _);
                remaining_count = remaining_count.wrapping_sub(power_of_two);
                current = current.add(power_of_two as _);
            }

            power_of_two <<= 1;
        }
    }

    pub unsafe fn add_free_blocks(&mut self, block: *mut BlockMeta, count: usize) {
        if self.coalescing_humongous_block.0.is_null() {
            self.coalescing_humongous_block.0 = block;
            self.coalescing_humongous_block.1 = block.add(count);
        } else if self.coalescing_humongous_block.1 == block {
            self.coalescing_humongous_block.1 = block.add(count);
        } else {
            let size = self.coalescing_humongous_block.1.offset_from(self.coalescing_humongous_block.0) as usize;
            self.add_free_blocks_internal(self.coalescing_humongous_block.0, size);
            self.coalescing_humongous_block.0 = block;
            self.coalescing_humongous_block.1 = block.add(count);
        }

        self.free_block_count += 1;
    }

    pub unsafe fn sweep_done(&mut self) {
        if self.coalescing_humongous_block.0.is_null() {
            return;
        }

        let size = self.coalescing_humongous_block.1.offset_from(self.coalescing_humongous_block.0) as usize;
        self.add_free_blocks_internal(self.coalescing_humongous_block.0, size);

        self.coalescing_humongous_block.0 = null_mut();
        self.coalescing_humongous_block.1 = null_mut();
    }
}