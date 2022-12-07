use std::sync::atomic::AtomicUsize;

use parking_lot::lock_api::RawMutex;

use crate::object::HeapObjectHeader;

use super::{heap::{Heap, HeapRegionClosure, heap}, marking::MarkingContext, region::HeapRegion};

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
                
                continue;
            }

            let header_address = header as usize;

            if start_of_gap != header_address {
                let new_free_list_entry_size = header_address - start_of_gap;

                (*region).free_list.add(start_of_gap as _, new_free_list_entry_size);
                (*region).largest_free_list_entry = std::cmp::max((*region).largest_free_list_entry, new_free_list_entry_size);
            }
            (*header).clear_marked();
            (*region).object_start_bitmap.set_bit(header as _);
            begin += size;
            start_of_gap = begin;
        }

        if start_of_gap != (*region).bottom() && start_of_gap != end {
            let size = (*region).bottom() + self.heap.options().region_size_bytes - start_of_gap;
            (*region).free_list.add(start_of_gap as _, size);
            (*region).largest_free_list_entry = std::cmp::max((*region).largest_free_list_entry, size);
        }

        start_of_gap == (*region).bottom()
    } 
}

unsafe impl<'a> Send for SweepGarbageClosure {}

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