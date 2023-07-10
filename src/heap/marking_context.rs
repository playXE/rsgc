use std::sync::atomic::AtomicUsize;

use crate::{system::object::HeapObjectHeader, system::traits::Visitor, utils::taskqueue::*};

use super::{
    bitmap::HeapBitmap,
    heap::heap,
    mark::{MarkQueueSet, MarkTask},
    memory_region::MemoryRegion,
    region::HeapRegion,
    shared_vars::SharedFlag,
};

#[repr(C)]
pub struct MarkingContext {
    mark_bit_map: HeapBitmap<16>,
    is_complete: SharedFlag,
    mark_queues: MarkQueueSet,
    visit_count: AtomicUsize,
}

impl MarkingContext {
    pub fn new(heap_region: MemoryRegion, max_queues: usize) -> Self {
        let this = Self {
            is_complete: SharedFlag::new(),
            mark_bit_map: HeapBitmap::new(heap_region.start(), heap_region.size()),
            mark_queues: MarkQueueSet::new(max_queues),
            visit_count: AtomicUsize::new(0),
        };

        this
    }

    pub fn mark_bitmap(&self) -> &HeapBitmap<16> {
        &self.mark_bit_map
    }
    pub fn mark_queues(&self) -> &MarkQueueSet {
        &self.mark_queues
    }

    pub fn is_marked(&self, obj: *const HeapObjectHeader) -> bool {
        self.mark_bit_map.check_bit(obj as _)
    }

    pub fn is_marked_range(&self, start: usize, end: usize) -> bool {
        self.mark_bit_map.is_marked_range(start, end)
    }

    /// Marks the object. Returns true if the object has not been marked before and has
    /// been marked by this thread. Returns false if the object has already been marked,
    /// or if a competing thread succeeded in marking this object.
    #[inline]
    pub fn mark(&self, obj: *const HeapObjectHeader) -> bool {
        self.mark_bit_map.atomic_test_and_set(obj as _)
    }

    #[inline]
    pub fn fetch_mark(&self, obj: *const HeapObjectHeader) {
        self.mark_bit_map.set_bit(obj as _);
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

impl Visitor for MarkingContext {
    unsafe fn visit(&mut self, object: *const u8) {
        let obj = (object as *const HeapObjectHeader).sub(1);
        if self.mark(obj) {
            self.visit_count.fetch_add(1, atomic::Ordering::Relaxed);
            self.mark_queues()
                .injector()
                .push(MarkTask::new(obj as _, false, false));
        }
    }

    unsafe fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize) {
        let heap = heap();

        for i in 0..len {
            let ptr = *ptrs.add(i);
            if heap.is_in(ptr as _) {
                let start = heap.object_start(ptr as _);
                if !start.is_null() {
                    self.visit(start.add(1).cast());
                }
            }
        }
    }

    fn visit_count(&self) -> usize {
        self.visit_count.load(atomic::Ordering::Relaxed)
    }
}
