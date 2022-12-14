use std::ptr::null_mut;

use crossbeam_queue::SegQueue;
use parking_lot::lock_api::RawMutex;

use crate::{heap::align_down, formatted_size};

use super::{
    bitmap::CHeapBitmap,
    free_list::FreeList,
    heap::Heap,
    region::{HeapRegion, HeapOptions, RegionState},
    AllocRequest, DynBitmap, sweeper::Sweep,
};

pub struct RegionFreeSet {
    heap: *mut Heap,
    pub(crate) mutator_free_bitmap: DynBitmap,
    mutator_leftmost: usize,
    mutator_rightmost: usize,
    max: usize,
    capacity: usize,
    used: usize,
    free_list: FreeList,
}

impl RegionFreeSet {
    pub fn used(&self) -> usize {
        self.used
    }
    pub fn heap(&self) -> &mut Heap {
        unsafe { &mut *self.heap }
    }

    pub(crate) fn set_heap(&mut self, heap: *mut Heap) {
        self.heap = heap;
        self.mutator_free_bitmap = DynBitmap::contained(self.heap().options().region_count);
        self.max = self.heap().options().region_count;
    }

    pub fn increase_used(&mut self, count: usize) {
        self.used += count;
    }

    pub fn decrease_used(&mut self, count: usize) {
        self.used -= count;
    }


    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn new(opts: &HeapOptions) -> Self {
        Self {
            heap: null_mut(),
            mutator_free_bitmap: DynBitmap::contained(0),
            mutator_leftmost: 0,
            mutator_rightmost: 0,
            max: 0,
            capacity: 0,
            used: 0,
            free_list: FreeList::new(opts),
        }
    }

    pub fn clear(&mut self) {
        self.mutator_free_bitmap.clear();
        self.mutator_leftmost = self.max;
        self.mutator_rightmost = 0;
        self.capacity = 0;
        self.used = 0;
    }

    pub fn rebuild(&mut self) {
        self.clear();

        for i in 0..self.heap().num_regions() {
            unsafe {
                let region = self.heap().get_region(i);

                if (*region).is_alloc_allowed() || (*region).is_trash() {
                    if self.alloc_capacity(region) == 0 {
                        continue; // do not add regions that would surely fail allocation
                    }
                    self.capacity += self.alloc_capacity(region);

                    assert!(self.used <= self.capacity, "must not use more than we have");
                    self.mutator_free_bitmap.set(i, true);
                }
            }
        }

        self.recompute_bounds();
    }

    pub fn available(&self) -> usize {
        self.capacity - self.used
    }

    pub fn is_mutator_free(&self, ix: usize) -> bool {
        self.mutator_free_bitmap.get(ix)
    }

    pub fn recompute_bounds(&mut self) {
        self.mutator_rightmost = self.max - 1;
        self.mutator_leftmost = 0;

        self.adjust_bounds();
    }

    pub fn adjust_bounds(&mut self) {
        while self.mutator_leftmost < self.max && !self.is_mutator_free(self.mutator_leftmost) {
            self.mutator_leftmost += 1;
        }

        while self.mutator_rightmost > 0 && !self.is_mutator_free(self.mutator_rightmost) {
            self.mutator_rightmost -= 1;
        }
    }

    pub fn mutator_count(&self) -> usize {
        self.mutator_free_bitmap.count_ones()
    }

    pub fn allocate(&mut self, req: &mut AllocRequest, in_new_region: &mut bool) -> *mut u8 {
        if req.size() > self.heap().options().humongous_threshold_bytes {
            *in_new_region = true;
            self.allocate_contiguous(req)
        } else {
            self.allocate_single(req, in_new_region)
        }
    }

    pub fn try_recycle_trashed(&mut self, r: *mut HeapRegion) {
        unsafe {
            if (*r).is_trash() {
                //self.heap().decrease_used((*r).size());
                (*r).recycle();
            }
        }
    }

    pub unsafe fn free_single(&mut self, addr: *mut u8, size: usize) {
        self.free_list.add(addr, size);
    }

    pub fn allocate_single(&mut self, req: &mut AllocRequest, in_new_region: &mut bool) -> *mut u8 {
        unsafe {
            // Scan the bitmap looking for a first fit.
            //
            // Leftmost and rightmost bounds provide enough caching to walk bitmap efficiently. Normally,
            // we would find the region to allocate at right away.

            for idx in self.mutator_leftmost..=self.mutator_rightmost {
                if self.is_mutator_free(idx) {
                    let result =
                        self.try_allocate_in(self.heap().get_region(idx), req, in_new_region);
                    if !result.is_null() {
                        return result;
                    }
                }
            }

            null_mut()
        }
    }

    unsafe fn try_allocate_in(
        &mut self,
        region: *mut HeapRegion,
        req: &mut AllocRequest,
        in_new_region: &mut bool,
    ) -> *mut u8 {
        assert!(!(*region).to_sweep);
        assert!(
            self.alloc_capacity(region) != 0,
            "Should avoid full regions on this path: {}",
            (*region).index()
        );

        self.try_recycle_trashed(region);

        *in_new_region = (*region).is_empty();
        let mut size = req.size();
        let result;
        if self.heap().options().elastic_tlab && req.for_lab() {
            let free = align_down((*region).peek_free(), 16);

            if size > free {
                size = free;
            }

            if size >= req.min_size() {
                result = (*region).allocate(size); // allocate from free-list
                if result.is_null() {
                    // region is fragmented and does not actually have hole large enough
                    return null_mut();
                }
            } else {
                result = null_mut();
            }
        } else {
            result = (*region).allocate(size); // allocate from free-list
        } 

        if !result.is_null() {
            self.increase_used(size);
            req.set_actual_size(size);

            if !req.for_lab() {
                // this is a regular allocation, not a TLAB and regular allocations need to be identified conservatively
                (*region).object_start_bitmap.set_bit(result as usize);
            }
        }

        if result.is_null() || self.alloc_capacity(region) == 0 {
            // Region cannot afford this or future allocations. Retire it.
            //
            // While this seems a bit harsh, especially in the case when this large allocation does not
            // fit, but the next small one would, we are risking to inflate scan times when lots of
            // almost-full regions precede the fully-empty region where we want allocate the entire TLAB.
            let waste = (*region).free();

            if waste > 0 {
                self.increase_used(waste);
                self.heap().notify_mutator_alloc(waste, true);
            }

            let num = (*region).index();

            self.mutator_free_bitmap.set(num, false);

            if self.touches_bounds(num) {
                self.adjust_bounds();
            }
        }

        result
    }

    pub fn touches_bounds(&self, num: usize) -> bool {
        num == self.mutator_leftmost || num == self.mutator_rightmost
    }

    pub fn allocate_contiguous(&mut self, req: &mut AllocRequest) -> *mut u8 {
        let size = req.size();
        let num = self.heap().options().required_regions(size);

        // No regions left to satisfy the allocation, bye
        if num > self.mutator_count() {
            return null_mut();
        }
        // Find the continuous interval of $num regions, starting from $beg and ending in $end,
        // inclusive. Contiguous allocations are biased to the beginning.
        let mut beg = self.mutator_leftmost;
        let mut end = beg;

        loop {
            if end >= self.max {
                // Hit the end, goodbye
                return null_mut();
            }

            // If regions are not adjacent, then current [beg; end] is useless, and we may fast-forward.
            // If region is not completely free, the current [beg; end] is useless, and we may fast-forward.
            if !self.is_mutator_free(end) || !self.can_allocate_from(self.heap().get_region(end)) {
                end += 1;
                beg = end;
                continue;
            }

            if (end - beg + 1) == num {
                break; // found the match
            }

            end += 1;
        }

        let remainder = size & self.heap().options().region_size_bytes_mask;

        for i in beg..=end {
            let r = self.heap().get_region(i);
            self.try_recycle_trashed(r);
            if i == beg {
                unsafe {
                    (*r).make_humonogous_start();
                }
            } else {
                unsafe {
                    (*r).make_humonogous_cont();
                }
            }
            unsafe { (*r).set_used(self.heap().options().region_size_bytes); }
            self.mutator_free_bitmap.set(i, false);
        }

        if beg == self.mutator_leftmost || end == self.mutator_rightmost {
            self.adjust_bounds();
        }

        self.used += self.heap().options().region_payload_size_bytes * num;

        req.set_actual_size(size);

        if remainder != 0 {
            self.heap()
                .notify_mutator_alloc(self.heap().options().region_size_bytes - remainder, true);
        }

        unsafe { (*self.heap().get_region(beg)).bottom() as _ }
    }

    /// Look at bitmap and return largest free bytes that are larger than minimal tlab size.
    ///
    /// # Notes
    ///
    /// Does not take into accoutn fragmentation of a region.
    ///
    /// # Safety
    ///
    /// Deliberately not locked, this method is unsafe when free set is modified.
    pub unsafe fn unsafe_peek_free(&self) -> usize {
        for index in self.mutator_leftmost..=self.mutator_rightmost {
            if index < self.max && self.is_mutator_free(index) {
                let r = self.heap().get_region(index);
                unsafe {
                    if (*r).peek_free() >= self.heap().options().min_tlab_size {
                        return (*r).peek_free();
                    }
                }
            }
        }

        0
    }

    pub fn can_allocate_from(&self, r: *mut HeapRegion) -> bool {
        unsafe { (*r).is_empty() || (*r).is_trash() }
    }

    pub fn alloc_capacity(&self, r: *mut HeapRegion) -> usize {
        unsafe {
            if (*r).is_trash() {
                self.heap().options().region_size_bytes
            } else {
                (*r).free()
            }
        }
    }

    pub fn recycle_trash(&mut self) {
        for i in 0..self.heap().options().region_count {
            let r = self.heap().get_region(i);
            unsafe {
                if (*r).is_trash() {
                    self.heap().lock.lock();
                    self.try_recycle_trashed(r);
                    self.heap().lock.unlock();
                }
            }
        }
    }

    /// External fragmentation metric: describes how fragmented the heap is.
    ///
    /// It is derived as:
    /// ```c
    ///     EF = 1 - largest_contiguous_free / total_free
    /// ```
    ///
    ///  For example:
    ///  a) Heap is completely empty => EF = 0
    ///  b) Heap is completely full => EF = 0
    ///  c) Heap is first-half full => EF = 1/2
    ///  d) Heap is half full, full and empty regions interleave => EF =~ 1
    pub fn external_fragmentation(&self) -> f64 {
        let mut last_idx = 0;
        let mut max_contig = 0;
        let mut empty_contig = 0;

        let mut free = 0;

        for index in self.mutator_leftmost..=self.mutator_rightmost {
            if self.is_mutator_free(index) {
                let r = self.heap().get_region(index);
                unsafe {
                    if (*r).is_empty() {
                        free += self.heap().options().region_size_bytes;

                        if last_idx + 1 == index {
                            empty_contig += 1;
                        } else {
                            empty_contig = 1;
                        }
                    } else {
                        empty_contig = 0;
                    }

                    max_contig = max_contig.max(empty_contig);
                    last_idx = index;
                }
            }
        }

        if free > 0 {
            return 1.0
                - (1.0 * max_contig as f64 * self.heap().options().region_size_bytes as f64
                    / free as f64);
        } else {
            return 0.0;
        }
    }

    /*
     * Internal fragmentation metric: describes how fragmented the heap regions are.
     *
     * It is derived as:
     *
     *               sum(used[i]^2, i=0..k)
     *   IF = 1 - ------------------------------
     *              C * sum(used[i], i=0..k)
     *
     * ...where k is the number of regions in computation, C is the region capacity, and
     * used[i] is the used space in the region.
     *
     * The non-linearity causes IF to be lower for the cases where the same total heap
     * used is densely packed. For example:
     *   a) Heap is completely full  => IF = 0
     *   b) Heap is half full, first 50% regions are completely full => IF = 0
     *   c) Heap is half full, each region is 50% full => IF = 1/2
     *   d) Heap is quarter full, first 50% regions are completely full => IF = 0
     *   e) Heap is quarter full, each region is 25% full => IF = 3/4
     *   f) Heap has one small object per each region => IF =~ 1
     */
    pub fn internal_fragmentation(&self) -> f64 {
        /*let mut squared = 0.0;
        let mut linear = 0.0;
        let mut count = 0;

        for index in self.mutator_leftmost..=self.mutator_rightmost {
            if self.is_mutator_free(index) {
                let r = self.heap().get_region(index);
                unsafe {
                    let used = (*r).used();
                    squared += used as f64 * used as f64;
                    linear += used as f64;
                    count += 1;
                }
            }
        }

        if count > 0 {
            let s = squared / (self.heap().options().region_size_bytes as f64 * linear);
            return 1.0 - s;
        } else {
            return 0.0;
        }*/

        let mut sum = 0.0;
        let mut count = 0;

        for index in self.mutator_leftmost..=self.mutator_rightmost {
            if self.is_mutator_free(index) {
                let r = self.heap().get_region(index);
                unsafe {
                    let frag = (*r).free_list.external_fragmentation();
                    sum += frag;
                    count += 1;
                }
            }
        }
        if sum > 0.0 {
            println!("sum {}", sum);
            sum / count as f64 
        } else {
            println!("zero sum");
            0.0
        }
    
    }

    pub fn log_status(&self) {
        if log::log_enabled!(target: "gc", log::Level::Debug) {
            let mut last_idx = 0;
            let mut max = 0;
            let mut max_contig = 0;
            let mut empty_contig = 0;

            let mut total_used = 0;
            let mut total_free = 0;
            let mut total_free_ext = 0;

            for idx in self.mutator_leftmost..=self.mutator_rightmost {
                unsafe { 
                    if self.is_mutator_free(idx) {
                        let r = self.heap().get_region(idx);
                        let free = self.alloc_capacity(r);

                        max = max.max(free);

                        if (*r).is_empty() {
                            total_free_ext += free;

                            if last_idx + 1 == idx {
                                empty_contig += 1;
                            } else {
                                empty_contig = 1;
                            }
                        } else {
                            empty_contig = 0;
                        }

                        total_used += (*r).used();
                        total_free += free;

                        max_contig = max_contig.max(empty_contig);
                        last_idx = idx;
                    }
                }
            }

            let max_humongous = max_contig * self.heap().options().region_size_bytes;

            let frag_ext =if total_free_ext > 0 {
                100 - (100 * max_humongous / total_free_ext)
            } else {
                0
            };

            let frag_int = if self.mutator_count() > 0 {
                100 * (total_used / self.mutator_count()) / self.heap().options().region_size_bytes
            } else {
                0
            };
            log::debug!(
                target: "gc",
                "Free: {}, Max: {} regular, {} humongous, Frag: {:.02}% external, {:.02}% internal",
                formatted_size(total_free),
                formatted_size(max),
                formatted_size(max_humongous),
                frag_ext,
                frag_int,
            );
        }
    }
}
