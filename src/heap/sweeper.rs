//! Sweep phase of the garbage collector.
//! 
//! This phase is responsible for freeing memory that is no longer in use. Regions are swept in parallel
//! and the work is distributed between the worker threads. Sweep phase can be cancelled at any time if 
//! mutator threads need more memory and sweep phase can't keep up with the rate of allocation.

use std::{collections::HashSet, intrinsics::unlikely, ptr::null_mut, sync::atomic::AtomicUsize};

use crossbeam_queue::SegQueue;
use parking_lot::{lock_api::RawMutex, Mutex};

use crate::{formatted_size, system::object::HeapObjectHeader, utils::stack::Stack};

use super::{
    heap::{heap, Heap, HeapRegionClosure},
    region::HeapRegion,
};

pub struct SweepGarbageClosure {
    pub heap: &'static Heap,
    pub concurrent: bool,
    pub live: AtomicUsize,
    pub freed: AtomicUsize,
}

impl SweepGarbageClosure {
    pub unsafe fn sweep_region(heap: &Heap, region: *mut HeapRegion) -> bool {
        let marking_context = heap.marking_context();
        let mut begin = (*region).bottom();
        let mut start_of_gap = begin;
        let end = begin + heap.options().region_size_bytes;
        (*region).free_list.clear();
        (*region).largest_free_list_entry = 0;
        //(*region).invoke_destructors(marking_context);
        (*region).object_start_bitmap.clear();
        let mut used = 0;
        let mut free = 0;

        if marking_context.is_marked_range(begin, end) {
            while begin != end {
                let header = begin as *mut HeapObjectHeader;

                let size = (*header).heap_size();
                if (*header).is_free() {
                    begin += size;
                    continue;
                }

                if !marking_context.is_marked(header) {
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
        log::trace!(target: "gc-sweeper", "Sweeping region #{}:{:p} used: {} free: {} (previous diff: {}), max: {}", (*region).index(), (*region).bottom() as *mut u8, formatted_size(used), formatted_size(free),
            (*region).last_sweep_free as isize - free as isize,
            formatted_size((*region).largest_free_list_entry)
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
            if (*r).to_sweep {
                (*r).to_sweep = false;
                if (*r).is_humongous_start() {
                    let humongous_obj = (*r).bottom() as *mut HeapObjectHeader;

                    if !self.heap.marking_context().is_marked(humongous_obj) {
                        self.freed.fetch_add(
                            self.heap
                                .options()
                                .required_regions((*humongous_obj).heap_size()),
                            atomic::Ordering::AcqRel,
                        );
                        self.heap.trash_humongous_region_at(r);
                    } else {
                        self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    }
                } else if (*r).is_humongous_cont() {
                    self.live.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    // todo: assertion that the previous region is a humongous start and has live object
                } else if (*r).is_regular() {
                    if Self::sweep_region(self.heap, r) {
                        self.freed.fetch_add(1, atomic::Ordering::AcqRel);
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
