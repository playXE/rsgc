use core::fmt;
use std::{mem::size_of, ptr::null_mut, time::Instant};

use crate::{formatted_size, env::read_uint_from_env};

use super::{align_down, free_list::FreeList, bitmap::HeapBitmap};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RegionState {
    Regular,
    HumongousStart,
    HumongousCont,
    EmptyUncommited,
    EmptyCommited,
    Trash,
}

/// HeapRegion is a header for a region of memory that is used for allocation.
/// 
/// It is stored in side-table instead of being stored in the region itself.
/// 
/// # Region types
/// 
/// ## Regular
/// 
/// Regular region is a free-list region, this means allocation in it can happen only from free-list.
/// 
/// ## Humongous
/// 
/// Humongous region is used for large objects, there might be object larger than region, then there is a few contiguous 
/// regions after first humongous region.
/// 
/// ## Empty
/// 
/// Empty unallocated region. 
pub struct HeapRegion {
    typ: RegionState,
    index: usize,
    used: usize,
    /// Start of region memory
    begin: *mut u8,
    /// End of region memory
    end: *mut u8,
    /// If this region is regular, points to non-null pointer.
    /// 
    /// Used for conservative marking and dealing with interior pointers.
    object_start_bitmap: HeapBitmap<16>,
    free_list: FreeList,
}

pub struct HeapArguments {
    pub min_region_size: usize,
    pub max_region_size: usize,
    pub target_num_regions: usize,
    pub humongous_threshold: usize,
    pub region_size: usize,
    pub elastic_tlab: bool,
    pub min_tlab_size: usize,
    pub max_tlab_size: usize,
    pub max_heap_size: usize,
    pub min_free_threshold: usize,
    pub allocation_threshold: usize,
    pub guaranteed_gc_interval: usize,
    pub control_interval_min: usize,
    pub control_interval_max: usize,
    pub control_interval_adjust_period: usize
}

impl Default for HeapArguments {
    fn default() -> Self {
        /*Self {
            min_region_size: 0,
            max_region_size: 0,
            target_num_regions: 2048,
            humongous_threshold: 100,
            region_size: 0,
            elastic_tlab: false,
        }*/

        todo!()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct HeapOptions {
    pub region_size_bytes: usize,
    pub region_size_words: usize,
    pub region_size_bytes_shift: usize,
    pub region_size_bytes_mask: usize,
    pub region_size_words_shift: usize,
    pub region_size_words_mask: usize,
    pub region_payload_size_bytes: usize,
    pub region_payload_size_words: usize,
    pub region_count: usize,
    pub humongous_threshold_words: usize,
    pub humongous_threshold_bytes: usize,
    pub max_heap_size: usize,
    pub max_tlab_size: usize,
    pub min_tlab_size: usize,
    pub region_size_log2: usize,
    pub elastic_tlab: bool,
    pub min_free_threshold: usize,
    pub allocation_threshold: usize,
    pub guaranteed_gc_interval: usize,
    pub control_interval_min: usize,
    pub control_interval_max: usize,
    pub control_interval_adjust_period: usize
}

impl HeapOptions {
    pub const fn required_regions(&self, size: usize) -> usize {
        (size + self.region_size_bytes - 1) >> self.region_size_bytes_shift
    }
}

impl fmt::Display for HeapOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionOptions")
            .field("region_size_bytes", &formatted_size(self.region_size_bytes))
            .field("region_size_words", &self.region_size_words)
            .field("region_size_bytes_shift", &self.region_size_bytes_shift)
            .field("region_size_bytes_mask", &self.region_size_bytes_mask)
            .field("region_size_words_shift", &self.region_size_words_shift)
            .field("region_size_words_mask", &self.region_size_words_mask)
            .field("region_count", &self.region_count)
            .field("humongous_threshold_words", &self.humongous_threshold_words)
            .field("humongous_threshold_bytes", &formatted_size(self.humongous_threshold_bytes))
            .field("max_heap_size", &formatted_size(self.max_heap_size))
            .field("region_size_log2", &self.region_size_log2).finish()
    }
}

impl HeapRegion {
    pub unsafe fn new(loc: usize, index: usize, start: usize, opts: &HeapOptions) -> *mut Self {
        let p = loc as *mut Self;
        p.write(Self {
            typ: RegionState::EmptyUncommited,
            object_start_bitmap: HeapBitmap::empty(),
            begin: start as *mut u8,
            index,
            used: 0,    
            end: (start + opts.region_size_bytes) as *mut u8,
            free_list: FreeList::new(opts)
        });

        (*p).recycle();
        
        p 
    }

    pub fn allocate(&mut self, size: usize) -> *mut u8 {
        unsafe {
            let (result, sz) = self.free_list.allocate(size);

            if result.is_null() {
                return null_mut();
            }
            if sz > size {
                let remainder = result.add(size);
                let remainder_size = result.add(sz) as usize - remainder as usize;
                self.free_list.add(remainder, remainder_size);
            }
            self.used += size;

            result
        }
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn free(&self) -> usize {
        self.free_list.free()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn bottom(&self) -> usize {
        self.begin as usize
    }

    pub fn size(&self) -> usize {
        self.end as usize - self.begin as usize
    }

    pub fn state(&self) -> RegionState {
        self.typ 
    }

    pub fn set_state(&mut self, state: RegionState) {
        self.typ = state 
    }

    pub fn set_used(&mut self, bytes: usize) {
        self.used = bytes;
    }

    pub fn object_start_bitmap(&self) -> &HeapBitmap<16> {
        &self.object_start_bitmap 
    }

    pub fn object_start_bitmap_mut(&mut self) -> &mut HeapBitmap<16> {
        &mut self.object_start_bitmap 
    }

    pub fn contains(&self, ptr: *mut u8) -> bool {
        ptr >= self.begin && ptr < self.end 
    }

    pub fn is_empty(&self) -> bool {
        self.typ == RegionState::EmptyUncommited || self.typ == RegionState::EmptyCommited 
    }

    pub fn is_alloc_allowed(&self) -> bool {
        self.typ == RegionState::Regular || self.is_empty()
    }

    pub fn is_trash(&self) -> bool {
        self.typ == RegionState::Trash 
    }

    pub fn make_humonogous_start(&mut self) {
        self.typ = RegionState::HumongousStart;
        self.free_list.clear();
    }

    pub fn make_humonogous_cont(&mut self) {
        self.typ = RegionState::HumongousCont;
        self.free_list.clear();
    }

    pub fn recycle(&mut self) {
        self.free_list.clear();
        self.make_empty();
        self.object_start_bitmap.clear();
        self.used = 0;

        unsafe { self.free_list.add(self.bottom() as _, self.size()); }
    }

    pub fn make_trash(&mut self) {
        self.typ = RegionState::Trash;
    }

    pub fn make_empty(&mut self) {
        self.typ = RegionState::EmptyCommited;
    }

    pub const MIN_PAGE_SIZE: usize = 4 * 1024;
    pub const MIN_NUM_PAGES: usize = 10;
    pub const MAX_PAGE_SIZE: usize = 32 * 1024 * 1024;

    /// Setups heap region sizes and thresholds based on input parameters.
    /// 
    /// # Notes
    /// - `humongous_threshold` is a percentage of memory in region that could be used for large object, for example:
    /// if you have humongous_threshold set to 50 and your region size is 4KB then largest object that will be allocated
    /// using free-list is 2KB, objects larger than that get a separate humongous region(s).
    pub fn setup_sizes(
        mut max_heap_size: usize,
        min_region_size: Option<usize>,
        target_num_regions: Option<usize>,
        max_region_size: Option<usize>,
        humongous_threshold: Option<usize>,
        region_size: Option<usize>,
        elastic_tlab: bool,
        min_tlab_size: usize,
    ) -> HeapOptions {
        let mut opts = HeapOptions::default();
       

        let min_region_size = min_region_size.map(|x| {
            if x < Self::MIN_PAGE_SIZE {
                Self::MIN_PAGE_SIZE
            } else {
                x 
            }
        }).unwrap_or(Self::MIN_PAGE_SIZE);

        let target_num_regions = target_num_regions.unwrap_or(128);
        let max_region_size = max_region_size.unwrap_or(Self::MAX_PAGE_SIZE);

        if min_region_size > max_heap_size / Self::MIN_NUM_PAGES {
            panic!("Max heap size ({}) is too low to afford the minimum number of regions ({}) of minimum region size ({})",
                formatted_size(max_heap_size) ,Self::MIN_NUM_PAGES,formatted_size(min_region_size)
            );
        }

        let mut region_size = if let Some(region_size) = region_size {
            region_size
        } else {
            let mut region_size = max_heap_size / target_num_regions;
            region_size = region_size.max(min_region_size);
            region_size = max_region_size.min(region_size);
            region_size
        };
        
        
       
        region_size = super::align_usize(region_size, super::virtual_memory::page_size());
        max_heap_size = super::align_usize(max_heap_size, region_size);

        let region_size_log = (region_size as f64).log2() as usize;
        region_size = 1 << region_size_log;
        max_heap_size = super::align_usize(max_heap_size, region_size);
        opts.region_count = max_heap_size / region_size;
        opts.region_size_bytes = region_size;
        opts.region_size_words = region_size / size_of::<usize>();
        opts.region_size_bytes_mask = region_size - 1;
        opts.region_size_words_mask = region_size - 1;
        opts.region_size_bytes_shift = region_size_log;
        opts.region_size_words_shift = opts.region_size_bytes_shift - 3;
        opts.region_size_log2 = region_size_log;
        let humongous_threshold = humongous_threshold.unwrap_or(50);
        opts.humongous_threshold_words = opts.region_size_words * humongous_threshold / 100;
        opts.humongous_threshold_words = align_down(opts.humongous_threshold_words, 8);
        opts.humongous_threshold_bytes = opts.humongous_threshold_words * size_of::<usize>();
        opts.max_heap_size = max_heap_size;

        // The rationale for trimming TLAB sizes has to do with the size of regions
        // and wasteful memory usage. The worst case realizes when "answer" is "region size", which means
        // it could prematurely retire an entire region. Having smaller TLABs does not fix that completely, but reduces the probability of too 
        // wasteful region usage. With current divisor we will waste no more than 1/8 of region size in the worst
        // case.
        //
        // One example of problem that happens when TLAB size is a region size: 
        // Program has 8 mutators and heap size of 256M with 32M regions, if all 8 mutators were to allocate
        // at the same time nothing incredibly bad would not happen, each of threads would get 32M regions. 
        // But once program has to start another thread it would end up in OOMing since no free memory is left.
        opts.max_tlab_size = (opts.region_size_bytes / 8).min(opts.humongous_threshold_bytes);
        opts.min_tlab_size = min_tlab_size;
        opts.max_tlab_size = opts.min_tlab_size.max(opts.max_tlab_size);
        opts.elastic_tlab = elastic_tlab;        

        log::info!(target: "gc", "Region sizes setup complete");
        log::info!(target: "gc", "- Max heap size: {}", formatted_size(opts.max_heap_size));
        log::info!(target: "gc", "- Region count: {}", opts.region_count);
        log::info!(target: "gc", "- Region size: {}", formatted_size(opts.region_size_bytes));
        log::info!(target: "gc", "- Humongous threshold: {}", formatted_size(opts.humongous_threshold_bytes));
        log::info!(target: "gc", "- Max TLAB size: {}", formatted_size(opts.max_tlab_size));
 
        opts 
    }

}
