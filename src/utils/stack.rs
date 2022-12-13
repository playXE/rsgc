use std::{mem::size_of, ptr::null_mut};

use crate::heap::align_up;

pub struct Stack<E: Copy> {
    seg_size: usize,
    max_size: usize,
    max_cache_size: usize,
    cur_seg_size: usize,
    full_seg_size: usize,
    cache_size: usize,
    cur_seg: *mut E,
    cache: *mut E,
}

impl<E: Copy> Stack<E> {
    pub const DEFAULT_SEGMENT_SIZE: usize = (4096 - 2 * size_of::<*mut E>()) / size_of::<E>();

    pub fn is_empty(&self) -> bool {
        self.cur_seg.is_null()
    }

    pub fn is_full(&self) -> bool {
        self.full_seg_size == self.max_size()
    }

    pub fn size(&self) -> usize {
        if self.is_empty() {
            0
        } else {
            self.full_seg_size + self.cur_seg_size
        }
    }

    pub const fn segment_size(&self) -> usize {
        self.seg_size
    }

    pub const fn max_size(&self) -> usize {
        self.max_size
    }

    pub const fn max_cache_size(&self) -> usize {
        self.max_cache_size
    }

    pub const fn cache_size(&self) -> usize {
        self.cache_size
    }

    fn adjust_max_size(mut max_size: usize, seg_size: usize) -> usize {
        let limit = usize::MAX - (seg_size - 1);

        if max_size == 0 || max_size > limit {
            max_size = limit;
        }
        (max_size + (seg_size - 1)) / seg_size * seg_size
    }

    pub fn new(segment_size: usize, max_cache_size: usize, max_size: usize) -> Self {
        Self {
            seg_size: segment_size,
            max_cache_size,
            max_size: Self::adjust_max_size(max_size, segment_size),
            cur_seg: null_mut(),
            cur_seg_size: segment_size,
            cache: null_mut(),
            cache_size: 0,
            full_seg_size: 0,
        }
    }

    fn reset(&mut self, reset_cache: bool) {
        self.cur_seg_size = self.seg_size;
        self.full_seg_size = 0;
        self.cur_seg = null_mut();

        if reset_cache {
            self.cache_size = 0;
            self.cache = null_mut();
        }
    }

    fn link_offset(&self) -> usize {
        align_up(self.seg_size * size_of::<E>(), size_of::<E>())
    }

    fn link_addr(&self, seg: *mut E) -> *mut *mut E {
        unsafe { (seg.cast::<u8>().add(self.link_offset())).cast() }
    }

    fn segment_bytes(&self) -> usize {
        self.link_offset() + size_of::<E>()
    }

    fn get_link(&self, seg: *mut E) -> *mut E {
        unsafe { self.link_addr(seg).read() }
    }

    fn set_link(&self, seg: *mut E, link: *mut E) -> *mut E {
        unsafe {
            self.link_addr(seg).write(link);
            seg
        }
    }

    fn alloc(bytes: usize) -> *mut E {
        unsafe { libc::malloc(bytes).cast() }
    }

    fn free(seg: *mut E) {
        unsafe {
            libc::free(seg.cast());
        }
    }

    #[inline(never)]
    #[cold]
    fn push_segment(&mut self) {
        let next;

        if self.cache_size > 0 {
            next = self.cache;
            self.cache = self.get_link(next);
            self.cache_size -= 1;
        } else {
            next = Self::alloc(self.segment_bytes());
        }

        let at_empty_transition = self.is_empty();

        self.cur_seg = self.set_link(next, self.cur_seg);
        self.cur_seg_size = 0;
        self.full_seg_size += if at_empty_transition {
            0
        } else {
            self.seg_size
        };
    }

    #[inline(never)]
    #[cold]
    fn pop_segment(&mut self) {
        let prev = self.get_link(self.cur_seg);

        if self.cache_size < self.max_cache_size {
            self.cache = self.set_link(self.cur_seg, self.cache);
            self.cache_size += 1;
        } else {
            Self::free(self.cur_seg);
        }
        let at_empty_transition = prev == null_mut();
        self.cur_seg = prev;
        self.cur_seg_size = self.seg_size;
        self.full_seg_size -= if at_empty_transition {
            0
        } else {
            self.seg_size
        };
    }

    fn free_segments(&self, mut seg: *mut E) {
        while !seg.is_null() {
            let prev = self.get_link(seg);
            Self::free(seg);
            seg = prev;
        }
    }

    pub fn push(&mut self, item: E) {
        let mut index = self.cur_seg_size;
        if index == self.seg_size {
            self.push_segment();
            index = 0;
        }

        unsafe {
            self.cur_seg.add(index).write(item);
            self.cur_seg_size = index + 1;
        }
    }

    pub fn pop(&mut self) -> Option<E> {
        if self.is_empty() {
            return None;
        }

        self.cur_seg_size -= 1;
        let index = self.cur_seg_size;
        let result = unsafe {
            self.cur_seg.add(index).read()
        };
        if index == 0 {
            self.pop_segment()
        }

        Some(result)
    }

    pub fn clear(&mut self, reset_cache: bool) {
        self.free_segments(self.cur_seg);
        if reset_cache {
            self.free_segments(self.cache);
        }

        self.reset(reset_cache);
    }
}

impl<E: Copy> Drop for Stack<E> {
    fn drop(&mut self) {
        self.clear(true);
    }
}