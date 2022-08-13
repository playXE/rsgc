use crate::base::constants::*;
use crate::{
    base::utils::{round_down_to_power_of_two32, which_power_of_two32},
    memory::object_header::{SizeTag, VtableTag},
};
use std::{
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};

use super::{object_header::ObjectHeader, page::PAGE_SIZE_LOG2};
#[repr(C)]
pub struct FreeListElement {
    tags: AtomicU64,
    next: AtomicPtr<Self>,
}

impl FreeListElement {
    pub unsafe fn as_element(addr: usize, size: usize) -> *mut FreeListElement {
        let result = addr as *mut Self;
        assert!(size >= OBJECT_ALIGNMENT);
        let mut tags = 0;

        tags = SizeTag::update(tags, size);

        tags = VtableTag::update(0, tags);

        (*result).tags = AtomicU64::new(tags);

        if size > SizeTag::MAX_SIZE_TAG as usize {
            (*result).size_address().write(size);
        }
        debug_assert_eq!(size, (*result).heap_size());
        (*result).next.store(null_mut(), Ordering::Relaxed);
        result
    }

    pub fn header_size_for(size: usize) -> usize {
        if size == 0 {
            0
        } else {
            (if size >= SizeTag::MAX_SIZE_TAG as usize {
                3
            } else {
                2
            }) * size_of::<usize>()
        }
    }

    pub fn set_next(&self, next: *mut FreeListElement) {
        self.next.store(next, Ordering::Relaxed);
    }

    pub fn link(&mut self, next: &mut *mut FreeListElement) {
        self.set_next(*next);
        *next = self as *const Self as *mut Self;
    }

    pub fn unlink(&mut self, next: &mut *mut FreeListElement) {
        *next = self.next.load(Ordering::Relaxed);
        self.next.store(null_mut(), Ordering::Relaxed);
    }

    pub fn next(&self) -> *mut Self {
        self.next.load(Ordering::Relaxed)
    }
    pub fn next_address(&self) -> *mut usize {
        &self.next as *const AtomicPtr<_> as _
    }
    pub fn size_address(&self) -> *mut usize {
        let addr = (&self.next as *const AtomicPtr<Self> as usize) + size_of::<usize>();

        addr as _
    }

    pub fn heap_size(&self) -> usize {
        self.heap_size_tags(self.tags.load(Ordering::Relaxed))
    }

    pub fn heap_size_tags(&self, tags: u64) -> usize {
        let size = SizeTag::decode(tags);

        if size != 0 {
            return size;
        }

        unsafe { self.size_address().read() }
    }
}

pub struct FreeList {
    free_list_heads: [*mut FreeListElement; PAGE_SIZE_LOG2],
    free_list_tails: [*mut FreeListElement; PAGE_SIZE_LOG2],
    biggest_free_list_index: usize,
}

#[inline(always)]
const fn bucket_for_size(size: u32) -> u32 {
    which_power_of_two32(round_down_to_power_of_two32(size))
}

impl FreeList {
    pub const fn new() -> Self {
        Self {
            free_list_heads: [null_mut(); PAGE_SIZE_LOG2],
            free_list_tails: [null_mut(); PAGE_SIZE_LOG2],
            biggest_free_list_index: 0,
        }
    }

    pub unsafe fn add(&mut self, block: Block) {
        self.add_returning_unused_blocks(block);
    }

    pub unsafe fn add_returning_unused_blocks(&mut self, block: Block) -> (*mut u8, *mut u8) {
        let size = block.size;

        if size < size_of::<FreeListElement>() {
            filler(block.address as _, size);
            return (
                block.address.add(size_of::<ObjectHeader>()),
                block.address.add(size_of::<ObjectHeader>()),
            );
        }
        let entry = FreeListElement::as_element(block.address as _, size);
        let index = bucket_for_size(size as u32) as usize;

        (*entry).link(&mut self.free_list_heads[index]);
        self.biggest_free_list_index = self.biggest_free_list_index.max(index);

        if (*entry).next().is_null() {
            self.free_list_tails[index] = entry;
        } 

        (
            entry
                .cast::<usize>()
                .add(FreeListElement::header_size_for(size))
                .cast(),
            entry.cast::<u8>().add(size),
        )
    }

    pub unsafe fn allocate(&mut self, allocation_size: usize) -> Block {
        // Try reusing a block from the largest bin. The underlying reasoning
        // being that we want to amortize this slow allocation call by carving
        // off as a large a free block as possible in one go; a block that will
        // service this block and let following allocations be serviced quickly
        // by bump allocation.
        // bucket_size represents minimal size of entries in a bucket.
        let mut bucket_size = 1 << self.biggest_free_list_index;
        let mut index = self.biggest_free_list_index;
        while index > 0 {
            debug_assert!(
                self.is_consistent(index),
                "unconsistent free list at index {} (size {}), head: {:p}, tail: {:p}",
                index,
                bucket_size,
                self.free_list_heads[index],
                self.free_list_tails[index]
            );
            let entry = self.free_list_heads[index];
            if allocation_size > bucket_size {
                // Final bucket candidate; check initial entry if it is able
                // to service this allocation. Do not perform a linear scan,
                // as it is considered too costly.
                if entry.is_null() || (*entry).heap_size() < allocation_size {
                    break;
                }
            }

            if !entry.is_null() {
                if (*entry).next().is_null() {
                    debug_assert_eq!(
                        self.free_list_tails[index], entry,
                        "tail should be the same as head at index {} {:p}",
                        index, entry
                    );
                    self.free_list_tails[index] = null_mut();
                }

                (*entry).unlink(&mut self.free_list_heads[index]);
                self.biggest_free_list_index = index;

                return Block {
                    address: entry.cast(),
                    size: (*entry).heap_size(),
                };
            }
            index -= 1;
            bucket_size >>= 1;
        }

        self.biggest_free_list_index = index;
        Block {
            address: null_mut(),
            size: 0,
        }
    }

    pub fn clear(&mut self) {
        self.free_list_heads.fill(null_mut());
        self.free_list_tails.fill(null_mut());

        self.biggest_free_list_index = 0;
    }

    pub fn size(&self) -> usize {
        let mut size = 0;
        for mut entry in self.free_list_heads.iter().copied() {
            unsafe {
                while !entry.is_null() {
                    size += (*entry).heap_size();
                    entry = (*entry).next();
                }
            }
        }
        size
    }

    pub fn is_empty(&self) -> bool {
        self.free_list_heads.iter().all(|entry| entry.is_null())
    }

    pub unsafe fn is_consistent(&self, index: usize) -> bool {
        let first_cond = self.free_list_heads[index].is_null() && self.free_list_tails[index].is_null();
        let second_cond = !self.free_list_heads[index].is_null() && !self.free_list_tails[index].is_null()
            && (*self.free_list_tails[index]).next().is_null();

        first_cond || second_cond
    }
}

pub struct Block {
    pub address: *mut u8,
    pub size: usize,
}

#[inline(always)]
pub unsafe fn filler(addr: usize, size: usize) {
    let obj = addr as *mut ObjectHeader;
    let mut bits = SizeTag::encode(size);
    bits = VtableTag::update(0, bits);
    (*obj).bits = bits;
}
