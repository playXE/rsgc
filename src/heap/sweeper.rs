use std::{collections::HashSet, intrinsics::unlikely, sync::atomic::AtomicUsize};

use crossbeam_queue::SegQueue;
use parking_lot::{lock_api::RawMutex, Mutex};

use crate::{formatted_size, object::HeapObjectHeader, utils::stack::Stack};

use super::{
    heap::{heap, Heap, HeapRegionClosure},
    region::HeapRegion,
};

pub struct SweepGarbageClosure {
    pub heap: &'static Heap,
    pub concurrent: bool,
    pub live: AtomicUsize,
}

impl SweepGarbageClosure {
    pub unsafe fn sweep_region(heap: &Heap, region: *mut HeapRegion) -> bool {
        let mut begin = (*region).bottom();
        let mut start_of_gap = begin;
        let end = begin + heap.options().region_size_bytes;
        (*region).free_list.clear();
        (*region).object_start_bitmap.clear();
        let mut used = 0;
        let mut free = 0;
        let marking_context = heap.marking_context();
        while begin != end {
            let header = begin as *mut HeapObjectHeader;

            let size = (*header).heap_size();
            assert!(size != 0);
            if (*header).is_free() {
                begin += size;
                continue;
            }

            if !marking_context.is_marked(header) {
                if unlikely((*header).vtable().light_finalizer) {
                    ((*header).vtable().finalize)(header.add(1).cast());
                }
                begin += size;
                continue;
            }

            let header_address = header as usize;

            if start_of_gap != header_address {
                let new_free_list_entry_size = header_address - start_of_gap;
                free += new_free_list_entry_size;
                (*region)
                    .free_list
                    .add(start_of_gap as _, new_free_list_entry_size);
                (*region).largest_free_list_entry =
                    std::cmp::max((*region).largest_free_list_entry, new_free_list_entry_size);
            }

            used += size;
            (*region).object_start_bitmap.set_bit(header as _);
            begin += size;

            start_of_gap = begin;
        }

        if start_of_gap != end {
            let size = (*region).bottom() + heap.options().region_size_bytes - start_of_gap;
            (*region).free_list.add(start_of_gap as _, size);
            (*region).largest_free_list_entry =
                std::cmp::max((*region).largest_free_list_entry, size);
            free += size;
        }
        (*region).set_used(used);
        assert!(used != 0 || free != 0);
        log::trace!(target: "gc-sweeper", "Sweeping region #{}:{:p} used: {} free: {} (previous diff: {})", (*region).index(), (*region).bottom() as *mut u8, formatted_size(used), formatted_size(free),
            (*region).last_sweep_free as isize - free as isize
        );
        (*region).last_sweep_free = free;

        start_of_gap == (*region).bottom()

        /*let mut used_in_bytes = 0;
        let mut free = 0;
        let start = (*region).bottom();
        let end = (*region).end();

        let mut current = start;
        (*region).object_start_bitmap.clear();
        (*region).free_list.clear();

        #[cfg(debug_assertions)]
        let mut free_set = HashSet::<usize>::new();

        while current < end {
            let raw_obj = current as *mut HeapObjectHeader;

            let mut obj_size = (*raw_obj).heap_size();

            if (*raw_obj).is_marked() {
                (*raw_obj).clear_marked();

                (*region).object_start_bitmap.set_bit(raw_obj as _);
                used_in_bytes += obj_size;
            } else {
                let mut free_end = current + obj_size;

                while free_end < end {
                    let next_obj = free_end as *mut HeapObjectHeader;
                    if (*next_obj).is_marked() {
                        break;
                    }
                    if !(*next_obj).is_free() {
                        if unlikely((*next_obj).vtable().light_finalizer) {
                            ((*next_obj).vtable().finalize)(next_obj.add(1).cast());
                        }
                    }
                    debug_assert!((*next_obj).heap_size() != 0x42 && (*next_obj).heap_size() != 0, "heap object {:p} with zero size ({:p} vt)", next_obj, (*next_obj).vtable());
                    free_end += (*next_obj).heap_size();
                }
                obj_size = free_end - current;

                #[cfg(debug_assertions)]
                {
                    core::ptr::write_bytes(current as *mut u8, 0x42, obj_size);
                    if !free_set.insert(current) {
                        panic!("double free: {:p}", current as *mut u8);
                    }
                }

                (*region).free_list.add(current as _, obj_size);
                (*region).largest_free_list_entry =
                    std::cmp::max((*region).largest_free_list_entry, obj_size);

                free += obj_size;
            }

            current += obj_size;
        }

        (*region).last_sweep_free = free;
        (*region).set_used(used_in_bytes);

        if used_in_bytes == 0 {
            assert!(free != 0, "no used objects in region means there is free memory");
        }
        used_in_bytes == 0*/
    }
}

unsafe impl<'a> Send for SweepGarbageClosure {}
unsafe impl<'a> Sync for SweepGarbageClosure {}
impl HeapRegionClosure for SweepGarbageClosure {
    fn heap_region_do(&self, r: *mut HeapRegion) {
        unsafe {
            if (*r).to_sweep {
                (*r).to_sweep = false;
                if (*r).is_humongous_start() {
                    let humongous_obj = (*r).bottom() as *mut HeapObjectHeader;

                    if !self.heap.marking_context().is_marked(humongous_obj) {
                        self.heap.trash_humongous_region_at(r);
                    } else {
                        self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    }
                } else if (*r).is_humongous_cont() {
                    self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    // todo: assertion that the previous region is a humongous start and has live object
                } else if (*r).is_regular() {
                    if Self::sweep_region(self.heap, r) {
                        (*r).make_trash();
                    } else {
                        self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    }
                }
            }
        }
    }
}

struct SweepTask<'a> {
    unswept: &'a SegQueue<usize>,
    freed: usize,
    live: &'a AtomicUsize,
}

impl<'a> SweepTask<'a> {
    fn new(unswept: &'a SegQueue<usize>, live: &'a AtomicUsize) -> Self {
        Self {
            unswept,
            freed: 0,
            live,
        }
    }

    fn run<const CANCELLABLE: bool>(&self) {
        let heap = heap();

        while let Some(region) = self.unswept.pop() {
            if CANCELLABLE && heap.check_cancelled_gc_and_yield(false) {
                return;
            }
            let region = region as *mut HeapRegion;
            unsafe {
                if (*region).is_humongous_start() {
                    let humongous_obj = (*region).bottom() as *mut HeapObjectHeader;
                    if !heap.marking_context().is_marked(humongous_obj) {
                        heap.trash_humongous_region_at(region);
                    } else {
                        self.live.fetch_add(
                            heap.options()
                                .required_regions((*humongous_obj).heap_size()),
                            atomic::Ordering::AcqRel,
                        );
                    }
                } else if (*region).is_humongous_cont() {
                    // todo: assertion that the previous region is a humongous start and has live object
                    continue;
                } else if (*region).is_regular() {
                    if SweepGarbageClosure::sweep_region(heap, region) {
                        (*region).make_trash();
                    } else {
                        self.live.fetch_add(1, atomic::Ordering::AcqRel);
                    }
                }
            }
        }
    }
}

pub struct Sweep {
    unswept: SegQueue<usize>,
    pub live: AtomicUsize,
}

impl Sweep {
    pub fn new() -> Self {
        Self {
            unswept: SegQueue::new(),
            live: AtomicUsize::new(0),
        }
    }

    pub fn add_region(&self, region: *mut HeapRegion) {
        self.unswept.push(region as _);
    }

    pub fn run<const CONCURRENT: bool>(&self) -> usize {
        let heap = heap();

        heap.workers().scoped(|scope| {
            let task = SweepTask {
                unswept: &self.unswept,
                live: &self.live,
                freed: 0,
            };
            scope.execute(move || {
                task.run::<CONCURRENT>();
            });
        });

        self.live.load(atomic::Ordering::Relaxed)
    }
}
