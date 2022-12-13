use crate::{object::HeapObjectHeader, utils::taskqueue::*};

use super::{mark_bitmap::MarkBitmap, shared_vars::SharedFlag, memory_region::MemoryRegion, region::HeapRegion, bitmap::HeapBitmap, taskqueue::{ObjToScanQueueSet, ObjToScanQueue}, mark::MarkQueueSet};

#[repr(C)]
pub struct MarkingContext {
    mark_bit_map: HeapBitmap<16>,
    is_complete: SharedFlag,
    mark_queues: MarkQueueSet
}

impl MarkingContext {
    pub fn new(heap_region: MemoryRegion, max_queues: usize) -> Self {
        let this = Self {
            is_complete: SharedFlag::new(),
            mark_bit_map: HeapBitmap::new(heap_region.start(), heap_region.size()),
            mark_queues: MarkQueueSet::new(max_queues)
        };

       
        this 
    }

    pub fn mark_queues(&self) -> &MarkQueueSet {
        &self.mark_queues
    }

    pub fn is_marked(&self, obj: *const HeapObjectHeader) -> bool {
        self.mark_bit_map.check_bit(obj as _)
    }

    /// Marks the object. Returns true if the object has not been marked before and has
    /// been marked by this thread. Returns false if the object has already been marked,
    /// or if a competing thread succeeded in marking this object.
    pub fn mark(&self, obj: *const HeapObjectHeader) -> bool {
        self.mark_bit_map.atomic_test_and_set(obj as _)
    }

    pub fn clear_bitmap(&self, r: *mut HeapRegion) {
        unsafe {
            let start = (*r).bottom();
            let end = (*r).end();


            self.mark_bit_map.clear_range_atomic(start, end);
        }
    }

    pub fn clear_bitmap_full(&mut self) {
        self.mark_bit_map.clear();
    }

    pub fn is_complete(&self) -> bool {
        self.is_complete.is_set()
    }

    pub fn mark_complete(&self) {
        self.is_complete.set();
    }

    pub fn mark_uncomplete(&self) {
        self.is_complete.unset();
    }
}
