use std::sync::atomic::AtomicUsize;

use parking_lot::lock_api::RawMutex;

use crate::{formatted_size, object::HeapObjectHeader};

use super::{
    heap::{heap, Heap, HeapRegionClosure},
    marking::MarkingContext,
    region::HeapRegion,
};

pub struct SweepGarbageClosure {
    pub heap: &'static Heap,
    pub concurrent: bool,
    pub live: AtomicUsize,
}

impl SweepGarbageClosure {
    pub unsafe fn sweep_region(&self, region: *mut HeapRegion) -> bool {
        let mut begin = (*region).bottom();
        let mut start_of_gap = begin;
        let end = begin + self.heap.options().region_size_bytes;
        (*region).free_list.clear();
        (*region).object_start_bitmap.clear();
        let mut used = 0;
        let mut free = 0;
        while begin != end {
            let header = begin as *mut HeapObjectHeader;

            let size = (*header).heap_size();
            assert!(size != 0);
            if (*header).is_free() {
                begin += size;

                continue;
            }

            if !(*header).is_marked() {
                begin += size;
                free += size;

                continue;
            }

            let header_address = header as usize;

            if start_of_gap != header_address {
                let new_free_list_entry_size = header_address - start_of_gap;

                (*region)
                    .free_list
                    .add(start_of_gap as _, new_free_list_entry_size);
                (*region).largest_free_list_entry =
                    std::cmp::max((*region).largest_free_list_entry, new_free_list_entry_size);
            }
            (*header).clear_marked();
            used += size;
            (*region).object_start_bitmap.set_bit(header as _);
            begin += size;

            start_of_gap = begin;
        }

        if start_of_gap != (*region).bottom() && start_of_gap != end {
            let size = (*region).bottom() + self.heap.options().region_size_bytes - start_of_gap;
            (*region).free_list.add(start_of_gap as _, size);
            (*region).largest_free_list_entry =
                std::cmp::max((*region).largest_free_list_entry, size);
            free += size;
        }
        (*region).set_used(used);
        log::trace!(target: "gc-sweeper", "Sweeping region #{}:{:p} used: {} free: {} (previous diff: {})", (*region).index(), (*region).bottom() as *mut u8, formatted_size(used), formatted_size(free),
            (*region).last_sweep_free as isize - free as isize
        );
        (*region).last_sweep_free = free;

        start_of_gap == (*region).bottom()
    }
}

unsafe impl<'a> Send for SweepGarbageClosure {}
unsafe impl<'a> Sync for SweepGarbageClosure {}
impl HeapRegionClosure for SweepGarbageClosure {
    fn heap_region_do(&self, r: *mut HeapRegion) {
        unsafe {
            if (*r).is_humongous_start() {
                let humongous_obj = (*r).bottom() as *mut HeapObjectHeader;

                if !(*humongous_obj).is_marked() {
                    self.heap.trash_humongous_region_at(r);
                } else {
                    self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    (*humongous_obj).clear_marked();
                }
            } else if (*r).is_humongous_cont() {
                self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                // todo: assertion that the previous region is a humongous start and has live object
            } else if (*r).is_regular() {
                if self.sweep_region(r) {
                    (*r).make_trash();
                } else {
                    self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                }
            }
        }
    }
}
