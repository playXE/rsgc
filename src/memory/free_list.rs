use crate::{
    base::bitset::BitSet,
    base::constants::*,
    memory::object_header::{SizeTag, VtableTag},
};

use std::{
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};

pub const NUM_LISTS: usize = 128;
pub const INITIAL_FREE_LIST_SEARCH_BUDGET: usize = 1000;

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
    top: usize,
    end: usize,

    unaccounted_size: usize,

    free_map: BitSet<{ NUM_LISTS as isize }>,

    free_lists: [*mut FreeListElement; NUM_LISTS + 1],

    freelist_search_budget: isize,

    last_free_small_size: isize,
}

impl FreeList {
    pub fn new() -> Self {
        let mut this = Self {
            top: 0,
            end: 0,
            unaccounted_size: 0,

            free_map: BitSet::new(),
            free_lists: [null_mut(); NUM_LISTS + 1],
            last_free_small_size: 0,
            freelist_search_budget: 1000,
        };

        unsafe {
            this.reset();
        }
        this
    }
    pub unsafe fn abandon_bump_allocation(&mut self) {
        if self.top < self.end {
            self.free(self.top, self.end - self.top);
            self.top = 0;
            self.end = 0;
        }
    }
    pub unsafe fn dequeue_element(&mut self, index: usize) -> *mut FreeListElement {
        let result = self.free_lists[index];
        let next = (*result).next();

        if next.is_null() && index != NUM_LISTS {
            let size = index << OBJECT_ALIGNMENT_LOG2;

            if size as isize == self.last_free_small_size {
                self.last_free_small_size = self.free_map.clear_last_and_find_previous(index as _)
                    * OBJECT_ALIGNMENT as isize;
            } else {
                self.free_map.set(index as _, false);
            }
        }

        self.free_lists[index] = next;

        result
    }

    pub const fn index_for_size(size: usize) -> usize {
        let index = size >> OBJECT_ALIGNMENT_LOG2;
        if index >= NUM_LISTS {
            NUM_LISTS
        } else {
            index
        }
    }

    pub unsafe fn try_allocate_bump_locked(&mut self, size: usize) -> usize {
        let result = self.top;
        let new_top = result + size;
        if new_top <= self.end {
            self.top = new_top;
            self.unaccounted_size += size;
            return result;
        }

        0
    }

    pub unsafe fn take_unaccounted_size_locked(&mut self) -> usize {
        let result = self.unaccounted_size;
        self.unaccounted_size = 0;
        result
    }

    pub unsafe fn try_allocate_small_locked(&mut self, size: usize) -> usize {
        if size as isize > self.last_free_small_size {
            return 0;
        }

        let index = Self::index_for_size(size);
        if index != NUM_LISTS && self.free_map.test(index as _) {
            return self.dequeue_element(index) as _;
        }

        if index + 1 < NUM_LISTS {
            let next_index = self.free_map.next(index as isize + 1);
            if next_index != -1 {
                let element = self.dequeue_element(next_index as _);

                return element as _;
            }
        }
        0
    }
    pub fn add_unaccounted_size(&mut self, size: usize) {
        self.unaccounted_size += size;
    }
    unsafe fn split_element_after_and_enqueue(
        &mut self,
        element: *mut FreeListElement,
        size: usize,
        is_protected: bool,
    ) {
       
        let remainder_size = (*element).heap_size() - size;
        if remainder_size == 0 {
            return;
        }

        let remainder_address = element as usize + size;
        let element = FreeListElement::as_element(remainder_address, remainder_size);
        let remainder_index = Self::index_for_size(remainder_size);
        self.enqueue_element(element, remainder_index);

        if is_protected {
            // TBD
        }
    }
    pub unsafe fn try_allocate_locked(&mut self, size: usize, is_protected: bool) -> usize {
        let index = Self::index_for_size(size);
        if index != NUM_LISTS && self.free_map.test(index as _) {
            let element = self.dequeue_element(index);

            return element as usize;
        }

        if index + 1 < NUM_LISTS {
            let next_index = self.free_map.next(index as isize + 1);

            if next_index != -1 {
                let element = self.dequeue_element(next_index as _);

                self.split_element_after_and_enqueue(element, size, is_protected);
                return element as _;
            }
        }

        let mut previous = null_mut::<FreeListElement>();
        let mut current = self.free_lists[NUM_LISTS];

        let mut tries_left = self.freelist_search_budget + (size >> WORD_SIZE_LOG2) as isize;

        while !current.is_null() {
            if (*current).heap_size() >= size {
                let remainder_size = (*current).heap_size() - size;
                let _region_size = size + FreeListElement::header_size_for(remainder_size);

                if previous.is_null() {
                    self.free_lists[NUM_LISTS] = (*current).next();
                } else {
                    (*previous).set_next((*current).next());
                }
                self.split_element_after_and_enqueue(current, size, is_protected);

                self.freelist_search_budget = tries_left.min(INITIAL_FREE_LIST_SEARCH_BUDGET as _);
                return current as _;
            } else if {
                let x = tries_left;
                tries_left -= 1;
                x < 0
            } {
                self.freelist_search_budget = INITIAL_FREE_LIST_SEARCH_BUDGET as _;
                return 0;
            }
            previous = current;
            current = (*current).next();
        }
        0
    }

    pub unsafe fn try_allocate(&mut self, size: usize, is_protected: bool) -> usize {
        let res = self.try_allocate_locked(size, is_protected);

        res
    }

    pub unsafe fn free_locked(&mut self, address: usize, size: usize) {
        let element = FreeListElement::as_element(address, size);
        let index = Self::index_for_size(size);
        self.enqueue_element(element, index);
    }
    pub unsafe fn reset(&mut self) {
        self.free_map.reset();
        self.last_free_small_size = -1;
        for i in 0..NUM_LISTS + 1 {
            self.free_lists[i] = null_mut();
        }
    }

    pub unsafe fn length_locked(&self, index: usize) -> usize {
        let mut current = self.free_lists[index];
        let mut count = 0;
        while !current.is_null() {
            count += 1;
            current = (*current).next();
        }
        count
    }

    pub unsafe fn make_iterable(&mut self) {
        if self.top < self.end {
            FreeListElement::as_element(self.top, self.end - self.top);
        }
    }

    pub unsafe fn try_allocate_large(&mut self, size: usize) -> *mut FreeListElement {
        let res = self.try_allocate_large_locked(size);

        res
    }

    pub unsafe fn try_allocate_large_locked(
        &mut self,
        minimum_size: usize,
    ) -> *mut FreeListElement {
        let mut previous = null_mut::<FreeListElement>();
        let mut current = self.free_lists[NUM_LISTS];

        let mut tries_left =
            self.freelist_search_budget + (minimum_size >> WORD_SIZE_LOG2) as isize;
        while !current.is_null() {
            let next = (*current).next();
            if (*current).heap_size() >= minimum_size {
                if previous.is_null() {
                    self.free_lists[NUM_LISTS] = next;
                } else {
                    (*previous).set_next(next);
                }
                self.freelist_search_budget = tries_left.min(INITIAL_FREE_LIST_SEARCH_BUDGET as _);
                return current;
            } else if {
                let x = tries_left;
                tries_left -= 1;
                x < 0
            } {
                self.freelist_search_budget = INITIAL_FREE_LIST_SEARCH_BUDGET as _;
                return null_mut();
            }

            previous = current;
            current = (*current).next();
        }

        null_mut()
    }
    pub unsafe fn free(&mut self, address: usize, size: usize) {
        self.free_locked(address, size);
    }
    pub unsafe fn enqueue_element(&mut self, element: *mut FreeListElement, index: usize) {
        let next = self.free_lists[index];
        if next.is_null() && index != NUM_LISTS {
            self.free_map.set(index as _, true);
            self.last_free_small_size = self
                .last_free_small_size
                .max((index as isize) << OBJECT_ALIGNMENT_LOG2);
        }

        (*element).set_next(next);
        self.free_lists[index] = element;
    }
}
