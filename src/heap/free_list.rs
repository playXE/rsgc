use std::{mem::size_of, ptr::null_mut};

use crate::{
    formatted_size,
    heap::heap::heap,
    object::{HeapObjectHeader, SizeTag, VtableTag},
};

use super::{region::HeapOptions, round_down_to_power_of_two32, which_power_of_two};

fn bucket_index_for_size(size: usize) -> usize {
    which_power_of_two(round_down_to_power_of_two32(size as _) as _)
}

#[repr(C)]
struct Filler {
    hdr: HeapObjectHeader,
}

impl Filler {
    unsafe fn new(loc: *mut u8, size: usize) -> *mut Self {
        let hdr = loc.cast::<HeapObjectHeader>();
        (*hdr).set_vtable(0);
        (*hdr).set_heap_size(size);

        hdr.cast()
    }
}

pub struct FreeList {
    free_list_heads: Box<[*mut Entry]>,
    free_list_tails: Box<[*mut Entry]>,
    biggest_free_list_index: usize,
    count: usize,
    free_bytes: usize,
}

impl FreeList {
    pub fn new(page_opts: &HeapOptions) -> Self {
        let free_list_heads = vec![null_mut(); page_opts.region_size_log2 + 1];
        let free_list_tails = vec![null_mut(); page_opts.region_size_log2 + 1];

        Self {
            free_list_heads: free_list_heads.into_boxed_slice(),
            free_list_tails: free_list_tails.into_boxed_slice(),
            biggest_free_list_index: 0,
            count: 0,
            free_bytes: 0,
        }
    }

    pub unsafe fn add_returning_unused_bounds(
        &mut self,
        addr: *mut u8,
        size: usize,
    ) -> (*mut u8, *mut u8) {
        debug_assert!(heap().is_in(addr));
        assert!(size != 0);
        if size < size_of::<Entry>() {
            let filler = Filler::new(addr, size);
            return (filler.add(1).cast(), filler.add(1).cast());
        }

        let entry = Entry::as_entry(addr as _, size);
        let index = bucket_index_for_size(size);
        (*entry).link(&mut self.free_list_heads[index]);
        self.biggest_free_list_index = self.biggest_free_list_index.max(index);

        if (*entry).next.is_null() {
            self.free_list_tails[index] = entry;
        }
        self.count += 1;
        self.free_bytes += size;
 
        (entry.add(1).cast(), entry.cast::<u8>().add(size))
    }

    pub unsafe fn add(&mut self, addr: *mut u8, size: usize) {
        self.add_returning_unused_bounds(addr, size);
    }

    pub unsafe fn append(&mut self, other: &mut FreeList) {
        for index in 0..self.free_list_tails.len() {
            let other_tail = other.free_list_tails[index];
            let this_head = &mut self.free_list_heads[index];

            if !other_tail.is_null() {
                (*other_tail).next = *this_head;
                if this_head.is_null() {
                    self.free_list_tails[index] = other_tail;
                }

                *this_head = other.free_list_heads[index];
                other.free_list_heads[index] = null_mut();
                other.free_list_tails[index] = null_mut();
            }
        }
        self.count += other.count;
        self.free_bytes += other.free_bytes;
        self.biggest_free_list_index = self
            .biggest_free_list_index
            .max(other.biggest_free_list_index);
    }

    pub fn free(&self) -> usize {
        self.free_bytes
    }

    pub fn clear(&mut self) {
        self.free_bytes = 0;
        self.count = 0;
        self.biggest_free_list_index = 0;
        self.free_list_heads.fill(null_mut());
        self.free_list_tails.fill(null_mut());
    }

    pub unsafe fn allocate(&mut self, allocation_size: usize) -> (*mut u8, usize) {
        let mut bucket_size = 1 << self.biggest_free_list_index;
        let mut index = self.biggest_free_list_index;

        while index > 0 {
            debug_assert!(
                self.is_consistent(index),
                "unconsistent free list at index {}: {:p} {:p}: {}",
                index,
                self.free_list_heads[index],
                self.free_list_tails[index],
                if self.free_list_heads[index].is_null() {
                    format!("tails is not null: {:p}", self.free_list_tails[index])
                } else if self.free_list_tails[index].is_null() {
                    format!("heads is not null: {:p}", self.free_list_heads[index])
                } else {
                    format!(
                        "heads and tails are not null: {:p} {:p} but tail points to non-null next: {:p}",
                        self.free_list_heads[index], self.free_list_tails[index],
                        (*self.free_list_tails[index]).next
                    )
                }
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
                if (*entry).next.is_null() {
                    self.free_list_tails[index] = null_mut();
                }

                (*entry).unlink(&mut self.free_list_heads[index]);
                self.biggest_free_list_index = index;
                self.free_bytes -= (*entry).heap_size();
                self.count -= 1;
                return (entry.cast(), (*entry).heap_size());
            }

            index -= 1;
            bucket_size >>= 1;
        }

        self.biggest_free_list_index = index;
        (null_mut(), 0)
    }

    pub fn peek_free(&self) -> usize {
        let index = self.biggest_free_list_index;

        if index > 0 {
            let entry = self.free_list_heads[index];

            if !entry.is_null() {
                return unsafe { (*entry).heap_size() };
            }
        }

        0
    }

    pub fn is_consistent(&self, index: usize) -> bool {
        (self.free_list_heads[index].is_null() && self.free_list_tails[index].is_null())
            || (!self.free_list_heads[index].is_null()
                && !self.free_list_tails[index].is_null()
                && unsafe {
                    (*self.free_list_tails[index]).next.is_null() })
    }

    pub fn external_fragmentation(&self) -> f64 {
        let mut largest_free_block = 0;
        let mut total = 0;

        for i in 0..self.free_list_heads.len() {
            unsafe {
                let mut entry = self.free_list_heads[i];

                while !entry.is_null() {
                    let size = (*entry).heap_size();
                    total += size;
                    largest_free_block = largest_free_block.max(size);

                    entry = (*entry).next;
                }
            }
        }

        1.0 - (largest_free_block as f64 / total as f64)
    }
}

#[repr(C)]
pub struct Entry {
    tags: u64,
    next: *mut Entry,
}

impl Entry {
    pub unsafe fn as_entry(addr: usize, size: usize) -> *mut Self {
        let result = addr as *mut Self;
        let mut tags = 0;
        tags = SizeTag::update(tags, size);
        tags = VtableTag::update(0, tags);

        (*result).tags = tags;
        (*result).next = null_mut();

        if size > SizeTag::MAX_SIZE_TAG as usize {
            (*result).size_address().write(size);
        }

        (*result).next = null_mut();
        assert!((*result).heap_size() != 0);
        result
    }

    pub unsafe fn heap_size(&self) -> usize {
        let sz = SizeTag::decode(self.tags);
        if sz == 0 {
            self.size_address().read()
        } else {
            sz
        }
    }

    pub fn size_address(&self) -> *mut usize {
        let addr = (&self.next as *const _ as usize) + size_of::<usize>();

        addr as _
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

    unsafe fn link(&mut self, previous_next: &mut *mut Entry) {
        self.next = *previous_next;
        *previous_next = self as *mut Self;
    }

    unsafe fn unlink(&mut self, previous_next: &mut *mut Entry) {
        *previous_next = self.next;
        self.next = null_mut();
    }
}
