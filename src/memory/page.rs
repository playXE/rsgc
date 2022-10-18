use super::{
    free_list::{Block, FreeList},
    marker::GCMarker,
    object_header::{ConstVal, ObjectHeader, VTable, VT},
    object_start_bitmap::ObjectStartBitmap,
    traits::{Allocation, Trace, PersistentRoot},
    visitor::Visitor,
};
use crate::base::{
    constants::{ALLOCATION_GRANULARITY, ALLOCATION_MASK, OBJECT_ALIGNMENT, WORD_SIZE_LOG2},
    utils::{
        read_float_from_env, read_string_from_env, read_uint_from_env, round_up, TinyBloomFilter,
    },
    virtual_memory::{page_size, VirtualMemory},
};
use crate::base::{formatted_size, segmented_vec::SegmentedVec};
use std::{
    collections::HashSet,
    intrinsics::unlikely,
    panic::AssertUnwindSafe,
    ptr::{self, null_mut},
};
use std::{
    mem::size_of,
    ops::{Deref, DerefMut},
};
pub const PAGE_SIZE_LOG2: usize = 17;
pub const PAGE_SIZE: usize = 1 << PAGE_SIZE_LOG2;
pub const PAGE_OFFSET_MASK: usize = PAGE_SIZE - 1;
pub const PAGE_BASE_MASK: usize = !PAGE_OFFSET_MASK;
pub const PAGE_SIZE_MASK: usize = !(PAGE_SIZE - 1);
pub const LARGE_OBJECT_SIZE_THRESHOLD: usize = 64 * 1024;

#[allow(dead_code)]
const TRACE: bool = !false;

unsafe fn free_page(page: *mut Page) {
    let ptr = &(*page).memory as *const Box<VirtualMemory>;
    let _ = ptr.read();
}

#[repr(C)]
pub struct Page {
    pub(crate) next: *mut Page,
    pub(crate) object_end: usize,
    pub(crate) memory: Box<VirtualMemory>,
    pub(crate) executable: bool,
    pub(crate) large: bool,
}

impl Page {
    #[inline(always)]
    pub fn of(addr: usize) -> *mut Self {
        (addr & PAGE_SIZE_MASK) as _
    }

    pub fn object_end(&self) -> usize {
        self.object_end
    }

    pub fn set_object_end(&mut self, end: usize) {
        self.object_end = end;
    }

    pub fn object_start(&self) -> usize {
        self.memory.start()
            + if self.large {
                LargePage::OBJECT_START_OFFSET
            } else {
                NormalPage::OBJECT_START_OFFSET
            }
    }
}

pub struct NormalPage {
    base: Page,
    used_in_bytes: usize,
    pub(crate) bitmap: ObjectStartBitmap,
}

impl NormalPage {
    pub fn allocate() -> *mut Self {
        let mem =
            VirtualMemory::allocate_aligned(PAGE_SIZE, PAGE_SIZE, false, false, "normal page");
        match mem {
            Some(mem) => {
                let bitmap = ObjectStartBitmap::new(mem.start() + Self::OBJECT_START_OFFSET);
                let ptr = mem.address().cast::<Self>();

                unsafe {
                    ptr.write(Self {
                        base: Page {
                            memory: mem,
                            object_end: 0,
                            executable: false,

                            large: false,
                            next: ptr::null_mut(),
                        },

                        used_in_bytes: 0,
                        bitmap,
                    });
                }
                ptr
            }

            None => ptr::null_mut(),
        }
    }

}

impl Deref for NormalPage {
    type Target = Page;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for NormalPage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

pub struct LargePage {
    base: Page,
}

impl Deref for LargePage {
    type Target = Page;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for LargePage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl NormalPage {
    pub const OBJECT_START_OFFSET: usize =
        round_up(size_of::<Self>() as _, ALLOCATION_GRANULARITY as _) as usize;
}

impl LargePage {
    pub fn allocate(size: usize) -> *mut LargePage {
        let size = Self::size_in_words(size) << WORD_SIZE_LOG2;
        let mem = VirtualMemory::allocate_aligned(size, page_size(), false, false, "large page");

        match mem {
            Some(mem) => {
                let ptr = mem.address().cast::<Self>();
                unsafe {
                    ptr.write(Self {
                        base: Page {
                            memory: mem,
                            object_end: 0,
                            executable: false,
                            large: true,
                            next: ptr::null_mut(),
                        },
                    });
                    (*ptr).set_object_end((*ptr).object_start() + size);
                }
                ptr
            }
            None => null_mut(),
        }
    }

    pub fn size_in_words(size: usize) -> usize {
        round_up(
            size as isize + Self::OBJECT_START_OFFSET as isize,
            page_size() as _,
        ) as usize
            >> WORD_SIZE_LOG2
    }

    pub const OBJECT_START_OFFSET: usize =
        round_up(size_of::<Self>() as _, ALLOCATION_GRANULARITY as _) as usize;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CollectionScope {
    Full,
    Eden,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HeapType {
    Large,
    Small,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SweepMode {
    /// Synchronous sweep right after collection cycle.
    Synhcronous,
    /// Lazy sweep. Will sweep pages on allocation instead. Might improve latency
    Lazy,
    /// Concurrent + lazy sweep. Not yet implemented
    Concurrent,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PageSpaceConfig {
    /// Collection threshold will grow by `used_bytes_after_gc * heap_growth_factor`.
    pub heap_growth_factor: f64,
    /// threshold to avoid that the total heap size grows by a factor
    /// of heap_growth_factor at every collection: it can only grow at most by
    /// the following factor from one collection to the next. Used e.g when there is
    /// a sudden temporary peak in memory usage; this avoids the upper bound grows too fast.
    pub max_heap_growth_factor: f64,
    /// Minimal heap size. If remaining live bytes are < MIN_HEAP_SIZE then GC threshold is set to MIN_HEAP_SIZE.
    pub min_heap_size: usize,
    /// Max heap size. If next collection threshold is > MAX_HEAP_SIZE then GC threshold is set to MAX_HEAP_SIZE.
    /// If there is > HEAP_MAX bytes allocated MemoryError is thrown using panic_any()
    pub max_heap_size: Option<usize>,
    /// Sweep mode
    pub sweep_mode: SweepMode,
    /// Enable GC on safepoints.
    /// When this option is set to true user should manually invoke [Heap::safepoint](crate::memory::Heap::safepoint) function periodically.
    /// This option is useful when you want to control when GC happens.
    pub gc_on_safepoint: bool,
}

impl Default for PageSpaceConfig {
    fn default() -> Self {
        Self {
            heap_growth_factor: 1.82,
            max_heap_growth_factor: 1.4,
            min_heap_size: 8 * 1024 * 1024,
            max_heap_size: None,
            sweep_mode: SweepMode::Synhcronous,
            gc_on_safepoint: false,
        }
    }
}

impl PageSpaceConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        config.max_heap_growth_factor = match read_float_from_env("HEAP_GROWTH_FACTOR") {
            Some(factor) if factor > 1.0 => factor,
            _ => 1.4,
        };

        config.heap_growth_factor = match read_float_from_env("HEAP_GROWTH_FACTOR") {
            Some(factor) if factor > 1.0 => factor,
            _ => 1.82,
        };

        config.min_heap_size = match read_uint_from_env("HEAP_MIN_SIZE") {
            Some(size) => {
                if size <= PAGE_SIZE {
                    PAGE_SIZE + 1024
                } else {
                    size
                }
            }
            _ => 8 * 1024 * 1024,
        };
        config.max_heap_size = match read_uint_from_env("MAX_HEAP_SIZE") {
            Some(size) => {
                if size < config.min_heap_size {
                    None
                } else {
                    Some(round_up(size as _, PAGE_SIZE as _) as usize)
                }
            }
            _ => None,
        };

        config.sweep_mode = match read_string_from_env("SWEEP_MODE") {
            Some(mode) => match mode.to_lowercase().as_str() {
                "synchronous" => SweepMode::Synhcronous,
                "lazy" => SweepMode::Lazy,
                "concurrent" => SweepMode::Lazy,
                _ => SweepMode::Synhcronous,
            },
            _ => SweepMode::Synhcronous,
        };

        config.gc_on_safepoint = match read_string_from_env("GC_ON_SAFEPOINT") {
            Some(mode) => match mode.to_lowercase().as_str() {
                "true" => true,
                "false" => false,
                _ => false,
            },
            _ => false,
        };

        config
    }

    pub const fn gc_on_safepoint(&self) -> bool {
        self.gc_on_safepoint
    }

    pub const fn sweep_mode(self) -> SweepMode {
        self.sweep_mode
    }

    pub const fn max_heap_size(self) -> Option<usize> {
        self.max_heap_size
    }

    pub const fn min_heap_size(self) -> usize {
        self.min_heap_size
    }

    pub const fn heap_growth_factor(self) -> f64 {
        self.heap_growth_factor
    }

    pub const fn max_heap_growth_factor(self) -> f64 {
        self.max_heap_growth_factor
    }

    pub const fn with_sweep_mode(self, mode: SweepMode) -> Self {
        Self {
            sweep_mode: mode,
            ..self
        }
    }

    pub fn with_max_heap_size(self, size: Option<usize>) -> Self {
        Self {
            max_heap_size: size.map(|size| {
                if size <= self.min_heap_size {
                    self.min_heap_size
                } else {
                    if size <= PAGE_SIZE {
                        PAGE_SIZE + 1024
                    } else {
                        size
                    }
                }
            }),
            ..self
        }
    }

    pub const fn with_min_heap_size(self, size: usize) -> Self {
        Self {
            min_heap_size: if size <= PAGE_SIZE {
                PAGE_SIZE + 1024
            } else {
                size
            },
            ..self
        }
    }

    pub fn with_heap_growth_factor(self, factor: f64) -> Self {
        Self {
            heap_growth_factor: if factor > 1.0 { factor } else { 1.82 },
            ..self
        }
    }

    pub const fn with_max_heap_growth_factor(self, factor: f64) -> Self {
        Self {
            max_heap_growth_factor: factor,
            ..self
        }
    }
}

#[derive(Debug)]
pub struct PageSpaceController {
    pub used_bytes: usize,
    pub next_collection_initial: usize,
    pub next_collection_threshold: usize,

    pub size_after_last_collect: usize,

    pub heap_growth_factor: f64,
    pub heap_growth_factor_max: f64,
    pub heap_type: HeapType,
    pub min_heap_size: usize,
    pub max_heap_size: Option<usize>,
    pub bounded_heap_size: Option<usize>,
    pub sweep_mode: SweepMode,
    pub gc_count: usize,
    pub gc_on_safepoint: bool,
    pub swept_at_mutator_time: usize,
}

impl PageSpaceController {
    pub fn new(config: PageSpaceConfig) -> Self {
        /*let heap_type = match read_uint_from_env("HEAP_TYPE") {
            Some(1) => HeapType::Large,
            Some(0) => HeapType::Small,
            _ => HeapType::Small,
        };

        // Collection threshold.
        let heap_growth_factor = match read_float_from_env("HEAP_GROWTH_FACTOR") {
            Some(factor) if factor > 1.0 => factor,
            _ => 1.82,
        };
        // threshold to avoid that the total heap size grows by a factor
        // of heap_growth_factor at every collection: it can only grow at most by
        // the following factor from one collection to the next. Used e.g when there is
        // a sudden temporary peak in memory usage; this avoids the upper bound grows too fast.
        let max_heap_growth_factor = match read_float_from_env("MAX_HEAP_GROWTH_FACTOR") {
            Some(factor) if factor > 1.0 => factor,
            _ => 1.4,
        };
        // Minimal heap size. If remaining live bytes are < MIN_HEAP_SIZE then GC threshold is set to MIN_HEAP_SIZE.
        let min_heap_size = match read_uint_from_env("MIN_HEAP_SIZE") {
            Some(size) => {
                if size <= PAGE_SIZE {
                    PAGE_SIZE + 1024 // add some memory so GC does not run recursively if there is only 1 page
                } else {
                    size
                }
            }
            None => 32 * 1024 * 1024,
        };
        // Max heap size. If next collection threshold is > MAX_HEAP_SIZE then GC threshold is set to MAX_HEAP_SIZE.
        // If there is > HEAP_MAX bytes allocated MemoryError is thrown using panic_any()
        let max_heap_size = read_uint_from_env("HEAP_MAX").map(|size| {
            if size <= PAGE_SIZE {
                PAGE_SIZE + 1024
            } else {
                size
            }
        });*/

        let mut this = Self {
            size_after_last_collect: 0,

            heap_growth_factor: config.heap_growth_factor,
            heap_type: HeapType::Small,
            max_heap_size: config.max_heap_size,
            heap_growth_factor_max: config.max_heap_growth_factor,
            next_collection_initial: config.min_heap_size,
            next_collection_threshold: config.min_heap_size,
            min_heap_size: config.min_heap_size,
            bounded_heap_size: None,
            used_bytes: 0,
            gc_count: 0,
            sweep_mode: config.sweep_mode,
            swept_at_mutator_time: 0,
            gc_on_safepoint: config.gc_on_safepoint
        };

        this.set_major_threshold_from(0.0);

        this
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn next_collection_threshold(&self) -> usize {
        self.next_collection_threshold
    }
    

    pub fn should_sweep_synchronously(&self) -> bool {
        match self.sweep_mode {
            SweepMode::Synhcronous => true,
            _ => false,
        }
    }

    pub fn should_sweep_concurrently(&self) -> bool {
        self.sweep_mode == SweepMode::Concurrent
    }

    pub fn min_heap_size(&self) -> usize {
        self.min_heap_size
    }

    pub fn proportional_heap_size(&self, heap_size: usize) -> usize {
        (self.heap_growth_factor * heap_size as f64) as usize
    }

    pub fn update_allocation_limits(&mut self, visited: usize) {
        let current_heap_size = visited;

        self.size_after_last_collect = current_heap_size;
    }

    pub fn set_major_threshold_from(&mut self, mut threshold: f64) -> bool {
        let threshold_max = round_up(
            (self.next_collection_initial as f64 * self.heap_growth_factor_max) as _,
            PAGE_SIZE as _,
        ) as f64;
        if threshold > threshold_max {
            threshold = threshold_max;
        }

        if (threshold as usize) < self.min_heap_size {
            threshold = self.min_heap_size as f64;
        }
        let bounded;
        if let Some(max_heap_size) = self
            .max_heap_size
            .filter(|max_heap_size| threshold > *max_heap_size as f64)
        {
            threshold = max_heap_size as f64;
            bounded = true;
        } else {
            bounded = false;
        }

        let threshold = round_up(threshold as _, PAGE_SIZE as _) as usize + 1024;

        self.next_collection_initial = threshold as _;
        self.next_collection_threshold = threshold as _;
        bounded
    }

    pub fn set_max_heap_size(&mut self, size: Option<usize>) {
        self.max_heap_size = size;
        if let Some(size) = self.max_heap_size {
            if size > 0 {
                if size < self.next_collection_initial {
                    self.next_collection_initial = size;
                } else {
                    self.next_collection_threshold = size;
                }
            }
        }
    }
}

pub struct Pages {
    current_lab: LinearAllocationBuffer,
    freelist: FreeList,
    controller: PageSpaceController,
    pages: *mut NormalPage,
    pages_tail: *mut NormalPage,

    large_pages: *mut LargePage,
    large_pages_tail: *mut LargePage,

    sweep_pages: *mut NormalPage,
    sweep_large_pages: *mut LargePage,

    pages_set: HashSet<usize>,
    filter: TinyBloomFilter,

    pub(crate) weak_refs: SegmentedVec<*mut ObjectHeader>,
    pub(crate) destructible: SegmentedVec<*mut ObjectHeader>,
    pub(crate) finalizable: SegmentedVec<*mut ObjectHeader>,
    pub(crate) run_finalizers: Vec<*mut ObjectHeader>,
    pub(crate) weak_maps: SegmentedVec<*mut ObjectHeader>,
    pub(crate) finalize_lock: bool,
    pub(crate) trace_callbacks: Vec<Box<dyn PersistentRoot>>,
    pub(crate) max_heap_size_already_raised: bool,
    pub(crate) external_data: *mut u8
}

struct Roots {
    trace_callbacks: *mut Vec<Box<dyn PersistentRoot>>,
    finalizable: Vec<*mut ObjectHeader>,
}

unsafe impl Trace for Roots {
    fn trace(&self, visitor: &mut dyn Visitor) {
        unsafe {
            (*self.trace_callbacks).retain_mut(|root| {
                let live = root.is_live();
                if live {
                    root.visit(visitor);
                    true 
                } else {
                    false
                }
            });
        }

        for obj in self.finalizable.iter() {
            // mark objects that are to be finalized as live. Happens only if GC is triggered inside a finalizer.
            unsafe {
                visitor.visit_pointer(*obj);
            }
        }
    }
}
impl Pages {
    pub fn new(config: PageSpaceConfig, external_data: *mut u8) -> Self {
        Self {
            current_lab: LinearAllocationBuffer::new(0, 0),
            freelist: FreeList::new(),
            pages: ptr::null_mut(),
            pages_tail: ptr::null_mut(),
            large_pages: ptr::null_mut(),
            large_pages_tail: ptr::null_mut(),
            sweep_large_pages: ptr::null_mut(),
            sweep_pages: ptr::null_mut(),
            controller: PageSpaceController::new(config),
            pages_set: HashSet::with_capacity(8),
            filter: TinyBloomFilter::new(0),
            destructible: SegmentedVec::new(),
            finalizable: SegmentedVec::new(),
            weak_refs: SegmentedVec::new(),
            trace_callbacks: Vec::with_capacity(4),
            weak_maps: SegmentedVec::new(),
            run_finalizers: Vec::new(),
            max_heap_size_already_raised: false,
            finalize_lock: false,
            external_data
        }
    }

    pub fn fragmentation(&self) -> f64 {
        self.freelist.fragmentation()
    }

    pub fn controller(&self) -> &PageSpaceController {
        &self.controller
    }

    pub unsafe fn large_pages_statistics(&self) -> (usize, usize) {
        let mut large_pages_count = 0;
        let mut large_pages_size = 0;
        let mut large_pages = self.large_pages;
        while !large_pages.is_null() {
            large_pages_count += 1;
            large_pages_size += (*large_pages).memory.size();
            large_pages = (*large_pages).next.cast();
        }
        (large_pages_count, large_pages_size)
    }

    pub unsafe fn normal_pages_statistics(&self) -> (usize, usize) {
        let mut pages_count = 0;
        let mut pages_size = 0;
        let mut pages = self.pages;
        while !pages.is_null() {
            pages_count += 1;
            pages_size += (*pages).memory.size();
            pages = (*pages).next.cast();
        }
        (pages_count, pages_size)
    }

    pub unsafe fn sweep_pages_statistics(&self) -> (usize, usize) {
        let mut pages_count = 0;
        let mut pages_size = 0;
        let mut pages = self.sweep_pages;
        while !pages.is_null() {
            pages_count += 1;
            pages_size += (*pages).memory.size();
            pages = (*pages).next.cast();
        }
        (pages_count, pages_size)
    }

    unsafe fn add_page(&mut self, page: *mut NormalPage) {
        if self.pages.is_null() {
            self.pages = page;
        } else {
            (*self.pages_tail).next = page.cast();
        }
        self.pages_set.insert(page as usize);
        self.filter.add_bits(page as usize);
        self.pages_tail = page;
    }

    unsafe fn add_large_page(&mut self, page: *mut LargePage) {
        if self.large_pages.is_null() {
            self.large_pages = page;
        } else {
            (*self.large_pages_tail).next = page.cast();
        }
        self.filter.add_bits(page as usize);
        self.pages_set.insert(page as usize);
        self.large_pages_tail = page;
    }

    unsafe fn allocate_page(&mut self, link: bool) -> *mut NormalPage {
        let page = NormalPage::allocate();
        if page.is_null() {
            return null_mut();
        }
        if link {
            self.add_page(page);
        }

        (*page).set_object_end((*page).memory.end());

        page
    }

    unsafe fn allocate_large_page(&mut self, size: usize) -> *mut LargePage {
        let page = LargePage::allocate(size);
        if page.is_null() {
            return null_mut();
        }
        self.add_large_page(page);
        page
    }

    /// Sweeps page lazily until `needs` bytes are freed. When sweeping in synchronous mode `needs` is simply set to usize::MAX
    /// and sweeping continues up to object_end of page.
    unsafe fn sweep_page(
        &mut self,
        page: *mut NormalPage,
        largest_free: &mut usize,
        _needs: usize,
    ) -> usize {
        let end = (*page).object_end();
        let mut used_in_bytes = 0;
        let start = (*page).object_start();
        let mut current = start;

        let bitmap = &mut (*page).bitmap;
        let mut largest_free_list_entry = 0;
        bitmap.clear();

        while current < end {
            let raw_obj = current as *mut ObjectHeader;
            let mut obj_size = (*raw_obj).heap_size();
            if (*raw_obj).clear_visited() {
                bitmap.set_bit(raw_obj as _);
                used_in_bytes += obj_size;
            } else {
                let mut free_end = current + obj_size;
                while free_end < end {
                    let next_obj = free_end as *mut ObjectHeader;
                    if (*next_obj).is_visited() {
                        break;
                    }
                    free_end += (*next_obj).heap_size();
                }

                obj_size = free_end - current;

                if current != start || free_end != end {
                    // only add to freelist if not covering the whole page
                    self.freelist.add(Block {
                        address: current as _,
                        size: obj_size,
                    });
                    largest_free_list_entry = largest_free_list_entry.max(obj_size);
                }
            }
            current += obj_size;
        }
        *largest_free = largest_free_list_entry;
        (*page).used_in_bytes = used_in_bytes;
        used_in_bytes
        /*
        let bitmap = &mut (*page).bitmap;
        let mut largest_new_free_list_entry = 0;
        let mut live_bytes = 0;

        let mut start_of_gap = (*page).object_start();

        let mut begin = start_of_gap;

        let end = (*page).object_end();
        bitmap.clear();

        while begin != end {
            let header = begin as *mut ObjectHeader;
            let size = (*header).heap_size();
            if (*header).is_free() {
                begin += size;
                continue;
            }

            if !(*header).clear_visited() {
                begin += size;
                continue;
            }
            let header_address = header as usize;
            if start_of_gap != header_address {
                let new_free_list_entry_size = header_address - start_of_gap;
                self.freelist.add(Block {
                    address: start_of_gap as _,
                    size: new_free_list_entry_size,
                });

                largest_new_free_list_entry =
                    largest_new_free_list_entry.max(new_free_list_entry_size);
            }
            bitmap.set_bit(begin);
            begin += size;
            start_of_gap = begin;
            live_bytes += size;

        }

        if start_of_gap != (*page).object_start() && start_of_gap != (*page).object_end() {
            self.freelist.add(Block {
                address: start_of_gap as _,
                size: (*page).object_end() - start_of_gap,
            });
        }

        *largest_free = largest_new_free_list_entry;

        (*page).used_in_bytes += live_bytes;

        live_bytes*/
    }
    /// Sweeps normal pages until `size` bytes is available in freelist
    unsafe fn lazy_sweep(&mut self, size: usize) -> bool {
        while !self.sweep_pages.is_null() {
            let next = (*self.sweep_pages).next;
            let mut largest_free_list_entry = 0;
            let used_in_bytes =
                self.sweep_page(self.sweep_pages, &mut largest_free_list_entry, size);
            self.controller.swept_at_mutator_time += 1;
            if largest_free_list_entry >= size {
                let page = self.sweep_pages;
                self.sweep_pages = next.cast();
                /*println!(
                    "lazy sweep succeed for {} (entry {})",
                    size, largest_free_list_entry
                );*/
                self.add_page(page);
                return true;
            } else if used_in_bytes == 0 {
                self.controller.used_bytes -= PAGE_SIZE;
                free_page(self.sweep_pages.cast());
            } else {
                // page is still alive but does not have enough memory to allocate
                self.add_page(self.sweep_pages);
            }
            self.sweep_pages = next.cast();
        }

        false
    }

    unsafe fn try_allocate_in_fresh_large_page(&mut self, size: usize) -> usize {
        if self.controller.used_bytes + LargePage::size_in_words(size) << WORD_SIZE_LOG2
            >= self.controller.next_collection_threshold && !self.controller.gc_on_safepoint
        {
            return 0;
        }
        let page = self.allocate_large_page(size);
        if page.is_null() {
            return 0;
        }
        self.controller.used_bytes += (*page).memory.size();
        self.add_large_page(page);
        (*page).object_start()
    }
    #[inline(always)]
    unsafe fn set_bit(addr: usize) {
        let page = Page::of(addr).cast::<NormalPage>();
        (*page).bitmap.set_bit(addr as _);
    }

    unsafe fn replace_lab(&mut self, new_buffer: usize, new_size: usize) {
        let lab = &mut self.current_lab;

        if lab.size() != 0 {
            let start = lab.start();
            let size = lab.size();
            self.freelist.add(Block {
                address: start as _,
                size,
            });
        }

        lab.set(new_buffer, new_size);
    }

    unsafe fn out_of_line_allocate(&mut self, size: usize) -> usize {
        if size >= LARGE_OBJECT_SIZE_THRESHOLD {
            return self.try_allocate_in_fresh_large_page(size);
        }

        let request_size = size;

        if !self.refill_linear_allocation_buffer(request_size) {
            return 0; // reached GC threshold
        }
        self.allocate_object_inline(size)
    }

    unsafe fn refill_linear_allocation_buffer(&mut self, size: usize) -> bool {
        if self.refill_linear_allocation_buffer_from_free_list(size) {
            return true;
        }

        if self.lazy_sweep(size) {
            if self.refill_linear_allocation_buffer_from_free_list(size) {
                return true;
            }
        }
        if self.controller.used_bytes + PAGE_SIZE >= self.controller.next_collection_threshold && !self.controller.gc_on_safepoint {
            return false;
        }
        let new_page = self.allocate_page(true);
        self.controller.used_bytes += PAGE_SIZE;
        self.replace_lab(
            (*new_page).object_start(),
            (*new_page).object_end() - (*new_page).object_start(),
        );
        true
    }

    unsafe fn refill_linear_allocation_buffer_from_free_list(&mut self, size: usize) -> bool {
        let entry = self.freelist.allocate(size);
        if entry.address.is_null() {
            return false;
        }

        self.replace_lab(entry.address as _, entry.size);
        true
    }
    #[inline]
    unsafe fn allocate_object_inline(&mut self, size: usize) -> usize {
        let current_lab = &mut self.current_lab;
        if current_lab.size() < size {
            return self.out_of_line_allocate(size);
        }
        let raw = current_lab.allocate(size);
        Self::set_bit(raw);
        debug_assert_eq!(0, raw & ALLOCATION_MASK);

        raw
    }

    #[inline]
    unsafe fn try_allocate_internal(&mut self, size: usize) -> usize {
        let current_lab = &mut self.current_lab;
        let lab_size = current_lab.size();

        if unlikely(lab_size >= size || size >= LARGE_OBJECT_SIZE_THRESHOLD) {
            let object = self.allocate_object_inline(size);

            return object;
        }
        self.out_of_line_allocate(size)
    }

    pub unsafe fn try_allocate_raw(&mut self, size: usize) -> usize {
        self.try_allocate_internal(size)
    }

    unsafe fn sweep_regular(&mut self, verbose: bool) {
        let mut page = self.sweep_pages;
        let mut live = 0;
        let mut dead = 0;
        self.controller.swept_at_mutator_time = 0;
        while !page.is_null() {
            let next = (*page).next;
            let used = self.sweep_page(page, &mut 0, usize::MAX);
            if used != 0 {
                // if page has live objects we just add it to pages list
                self.add_page(page);
                live += 1;
                self.controller.used_bytes += PAGE_SIZE;
            } else {
                dead += 1;
                // otherwise we free it
                // todo: add free page pool
                free_page(page.cast());
            }
            page = next.cast();
        }
        self.sweep_pages = null_mut();
        if verbose {
            println!(
                "[GC({})] finish lazy sweep before collection, {} live, {} dead",
                self.controller.gc_count, live, dead
            );
        }
    }

    pub fn sweep(&mut self) {
        unsafe {
            self.pages_set.clear();
            self.sweep_large_pages = self.large_pages;
            self.large_pages = null_mut();
            self.filter.reset();
            self.large_pages_tail = null_mut();
            self.sweep_pages = self.pages;
            self.pages_tail = null_mut();
            self.pages = null_mut();
            self.controller.used_bytes = 0;
            self.freelist.clear();
            // large pages are always sweeped synchronously
            {
                // todo: add best-fit free list for large pages?
                let mut page = self.sweep_large_pages;
                while !page.is_null() {
                    let next = (*page).next;
                    let raw_obj = (*page).object_start() as *mut ObjectHeader;
                    if (*raw_obj).clear_visited() {
                        self.add_large_page(page);
                        self.controller.used_bytes += (*page).memory.size();
                    } else {
                        free_page(page.cast());
                    }
                    page = next.cast();
                }
                self.sweep_large_pages = null_mut();
            }

            // check if we should sweep right after marking
            if self.controller.should_sweep_synchronously() {
                self.sweep_regular(false);
            } else {
                let mut page = self.sweep_pages;
                while !page.is_null() {
                    // all pages are considered as live after marking when sweeping is lazy or concurrent
                    self.controller.used_bytes += PAGE_SIZE;

                    page = (*page).next.cast();
                }
            }
        }
    }

    pub fn try_pointer_conservative(&self, addr: usize) -> *mut ObjectHeader {
        let page = Page::of(addr);

        // first use tiny bloom filter
        if self.filter.rule_out(page as usize) {
            return null_mut();
        }

        let page_addr = page as usize;
        // second check our hashset to be 100% sure that this page is live
        if !self.pages_set.contains(&page_addr) {
            return null_mut();
        }

        unsafe {
            if (*page).large {
                // if page is large we simply return its object_start()
                (*page.cast::<LargePage>()).object_start() as *mut ObjectHeader
            } else {
                // normal pages contain ObjectStartBitmap that we can use to find object from raw address
                let page = page.cast::<NormalPage>();
                if addr < (*page).object_start() || addr >= (*page).object_end() {
                    return null_mut();
                }
                (*page).bitmap.find_header(addr)
            }
        }
    }

    #[inline(never)]
    pub fn collect(&mut self) {
        let verbose = read_uint_from_env("GC_VERBOSE");
        let time = if let Some(1) = verbose {
            println!(
                "[GC({})] start, used: {}",
                self.controller.gc_count,
                formatted_size(self.controller.used_bytes)
            );
            if self.controller.sweep_mode == SweepMode::Lazy {
                println!(
                    "\tswept at mutator time: {}",
                    self.controller.swept_at_mutator_time
                );
            }
            Some(std::time::Instant::now())
        } else {
            None
        };
        if self.controller.sweep_mode == SweepMode::Lazy {
            unsafe {
                // finish sweeping if needed
                self.sweep_regular(time.is_some());
            }
        }
        unsafe {
            self.replace_lab(0, 0);
        }
        let mut marker = GCMarker::new();
        let mut callbacks = std::mem::replace(&mut self.trace_callbacks, vec![]);
        let mut roots = Roots {
            finalizable: std::mem::replace(&mut self.run_finalizers, vec![]),
            trace_callbacks: &mut callbacks,
        };
        marker.mark(None, self as *const Self, &mut roots);
        let bytes = marker.marked_bytes();
        let time_micros = marker.marked_micros();
        let words_per_micro = (bytes / size_of::<usize>()) as u64 / time_micros as u64;
        self.run_finalizers.append(&mut roots.finalizable);
        self.trace_callbacks = callbacks;
        self.sweep();

        let bounded = self.controller.set_major_threshold_from(
            self.controller.used_bytes as f64 * self.controller.heap_growth_factor,
        );
        if bounded && self.controller.used_bytes >= self.controller.next_collection_initial {
            if self.max_heap_size_already_raised {
                eprintln!("using too much memory, aborting");
                std::process::abort();
            }

            self.max_heap_size_already_raised = true;
            std::panic::panic_any(MemoryError);
        }

        // last step is to execute finalizers. They are always executed *after* sweep phase.
        // If they would be executed right after marking there is a chance that they will allocate
        // extra memory and trigger GC. This is not a problem, but it is not nice.
        if !self.finalize_lock {
            self.finalize_lock = true;

            while let Some(obj) = self.run_finalizers.pop() {
                let _ = std::panic::catch_unwind(move || unsafe {
                    match (*obj).vtable().finalize {
                        Some(cb) => cb(obj.add(1).cast()),
                        None => (),
                    }
                });
            }
            self.finalize_lock = false;
        }
        if let Some(time) = time {
            eprintln!("[GC({})] end in {:.04}ms \n  marking: {:.4}ms,\n  mark words per micro: {}\n  heap usage: {}\n  next collection threshold: {}\n  next collection initial: {}", 
                self.controller.gc_count, 
                time.elapsed().as_micros() as f64 / 1000.0, 
                marker.marked_micros() as f64 / 1000.0, 
                words_per_micro, 
                formatted_size(self.controller.used_bytes), 
                formatted_size(self.controller.next_collection_threshold),
                formatted_size(self.controller.next_collection_initial)
            );
        }
        self.controller.gc_count += 1;
    }
    #[inline(never)]
    #[cold]
    unsafe fn collect_and_try_allocate(&mut self, size: usize) -> usize {
        self.collect();
        let result = self.try_allocate_internal(size);
        if result == 0 {
            std::panic::panic_any(MemoryError);
        }

        result
    }
    #[inline(never)]
    #[cold]
    fn post_malloc<A: 'static + Allocation>(&mut self, obj: *mut ObjectHeader) {
        if A::LIGHT_FINALIZER && A::FINALIZE {
            self.destructible.push(obj);
        } else if A::FINALIZE && !A::LIGHT_FINALIZER {
            self.finalizable.push(obj);
        }

        if A::HAS_WEAKPTR {
            self.weak_refs.push(obj);
        }
    }

    pub unsafe fn malloc_manual(&mut self, vtable: &'static VTable, size: usize ,has_gcptrs: bool, light_finalizer: bool, finalize: bool, has_weakptr: bool) -> *mut ObjectHeader {
        let size = round_up(size as isize + size_of::<ObjectHeader>() as isize, OBJECT_ALIGNMENT as _) as usize;
        let result = self.try_allocate_internal(size);
        if result == 0 {
            std::panic::panic_any(MemoryError);
        }
        let result = result as *mut ObjectHeader;
        (*result).set_vtable(vtable as *const VTable as usize);
        (*result).set_heap_size(size);
        (*result).clear_visited_unsync();
        
        if !has_gcptrs {
            (*result).set_no_heap_ptrs();
        }

        #[cold]
        fn post_malloc(heap: &mut Pages, light_finalizer: bool, finalize: bool, has_weakptr: bool, obj: *mut ObjectHeader) {
            if light_finalizer && finalize {
                heap.destructible.push(obj);
            } else if finalize && !light_finalizer {
                heap.finalizable.push(obj);
            }
    
            if has_weakptr {
                heap.weak_refs.push(obj);
            }
        }
        post_malloc(self, light_finalizer, finalize, has_weakptr,result);
        result 
    }

    pub fn malloc_fixedsize<A: 'static + Allocation>(&mut self) -> *mut ObjectHeader {
        let size = round_up(
            A::SIZE as isize + size_of::<ObjectHeader>() as isize,
            OBJECT_ALIGNMENT as _,
        ) as usize;

        unsafe {
            let mut result = self.try_allocate_internal(size);
            if result == 0 {
                result = self.collect_and_try_allocate(size);
            }
            let result = result as *mut ObjectHeader;
            (*result).set_vtable(VT::<A>::VAL as *const VTable as usize);
            (*result).set_heap_size(size);
            (*result).clear_visited_unsync();
            if !A::HAS_GCPTRS {
                (*result).set_no_heap_ptrs();
            }
            if A::LIGHT_FINALIZER | A::FINALIZE | A::HAS_WEAKPTR {
                self.post_malloc::<A>(result);
            }

            result
        }
    }

    pub fn malloc_varsize<A: 'static + Allocation>(&mut self, length: usize) -> *mut ObjectHeader {
        let size = size_of::<ObjectHeader>() + A::SIZE + (A::VARSIZE_ITEM_SIZE * length);
        let size = round_up(size as _, OBJECT_ALIGNMENT as _) as usize;

        unsafe {
            let mut result = self.try_allocate_internal(size);
            if unlikely(result == 0) {
                result = self.collect_and_try_allocate(size);
            }
            let result = result as *mut ObjectHeader;
            (*result).set_vtable(VT::<A>::VAL as *const VTable as usize);
            (*result).set_heap_size(size);
            (*result).clear_visited_unsync();
            if !A::HAS_GCPTRS {
                (*result).set_no_heap_ptrs();
            }
            let data = (*result).data() as *mut u8;
            data.add(A::VARSIZE_OFFSETOF_LENGTH)
                .cast::<usize>()
                .write(length);

            if A::LIGHT_FINALIZER | A::FINALIZE | A::HAS_WEAKPTR {
                self.post_malloc::<A>(result);
            }
            result
        }
    }
}

pub struct MemoryError;

unsafe fn free_pages(pages: *mut Page) {
    let mut p = pages;
    while !p.is_null() {
        let next = (*p).next;
        free_page(p);
        p = next;
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        unsafe {
            if let Some(true) = read_uint_from_env("COLLECT_ON_DROP").map(|val| val != 0) {
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    self.collect();
                }));
            }
            while let Some(obj) = self.destructible.pop() {
                let _ = std::panic::catch_unwind(move || match (*obj).vtable().finalize {
                    Some(cb) => cb(obj.add(1).cast()),
                    None => (),
                });
            }
            free_pages(self.pages.cast());
            free_pages(self.sweep_pages.cast());
            free_pages(self.large_pages.cast());
        }
    }
}

pub struct LinearAllocationBuffer {
    start: usize,
    size: usize,
}

impl LinearAllocationBuffer {
    #[inline(always)]
    pub fn allocate(&mut self, size: usize) -> usize {
        let result = self.start;
        self.start += size;
        self.size -= size;
        result
    }
    #[inline(always)]
    pub fn set(&mut self, start: usize, size: usize) {
        self.start = start;
        self.size = size;
    }
    #[inline(always)]
    pub const fn size(&self) -> usize {
        self.size
    }
    #[inline(always)]
    pub const fn start(&self) -> usize {
        self.start
    }

    pub const fn new(start: usize, size: usize) -> Self {
        LinearAllocationBuffer { start, size }
    }
}
