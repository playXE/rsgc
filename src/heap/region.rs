use core::fmt;
use std::intrinsics::unlikely;
use std::{mem::size_of, ptr::null_mut, time::Instant};

use crate::env::{get_total_memory, read_float_from_env};
use crate::heap::heap::heap;
use crate::system::object::HeapObjectHeader;
use crate::{env::read_uint_from_env, formatted_size};

use super::marking_context::MarkingContext;
use super::virtual_memory::{PlatformVirtualMemory, VirtualMemory, VirtualMemoryImpl};
use super::GCHeuristic;
use super::{align_down, bitmap::HeapBitmap, free_list::FreeList, virtual_memory};

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
    pub visited: bool,
    pub(crate) object_start_bitmap: HeapBitmap<16>,
    /// Start of region memory
    begin: *mut u8,
    /// End of region memory
    end: *mut u8,
    pub(crate) free_list: FreeList,
    pub(crate) largest_free_list_entry: usize,
    pub(crate) last_sweep_free: usize,
    pub empty_time: Instant,
    pub to_sweep: bool,
}

#[derive(Debug)]
pub struct HeapArguments {
    /// With automatic region sizing, the regions would be at most
    /// this large.
    pub max_region_size: usize,
    /// With automatic region sizing, the regions would be at least
    /// this large.
    pub min_region_size: usize,
    /// With automatic region sizing, this is the approximate number
    /// of regions that would be used, within min/max region size
    /// limits."
    pub target_num_regions: usize,
    /// Humongous objects are allocated in separate regions.
    /// This setting defines how large the object should be to be
    /// deemed humongous. Value is in  percents of heap region size.
    /// This also caps the maximum TLAB size.
    pub humongous_threshold: usize,
    /// Static heap region size. Set zero to enable automatic sizing.
    pub region_size: usize,
    pub elastic_tlab: bool,
    pub min_tlab_size: usize,
    pub tlab_size: usize,
    pub tlab_waste_target_percent: usize,
    pub tlab_refill_waste_fraction: usize,
    pub tlab_waste_increment: usize,
    pub max_heap_size: usize,
    pub min_heap_size: usize,
    pub initial_heap_size: usize,
    pub min_free_threshold: usize,
    pub allocation_threshold: usize,
    pub guaranteed_gc_interval: usize,
    pub control_interval_min: usize,
    pub control_interval_max: usize,
    pub control_interval_adjust_period: usize,
    pub uncommit: bool,
    pub uncommit_delay: usize,
    /// How many regions to process at once during parallel region
    /// iteration. Affects heaps with lots of regions.
    pub parallel_region_stride: usize,
    pub parallel_gc_threads: usize,
    pub min_ram_percentage: f64,
    pub max_ram_percentage: f64,
    pub initial_ram_percentage: f64,
    pub parallel_mark: bool,
    pub parallel_sweep: bool,
    pub mark_loop_stride: usize,
    pub max_satb_buffer_flushes: usize,
    pub max_satb_buffer_size: usize,
    pub full_gc_threshold: usize,
    /// Always do full GC cycle.
    pub always_full: bool,
    pub adaptive_decay_factor: f64,
    pub learning_steps: usize,
    pub immediate_threshold: usize,
    pub adaptive_sample_frequency_hz: usize,
    pub adaptive_sample_frequency_size_seconds: usize,
    pub adaptive_initial_confidence: f64,
    pub adaptive_initial_spike_threshold: f64,
    pub alloc_spike_factor: usize,

    /// GC heuristics to use. This fine-tunes the GC mode selected,
    /// by choosing when to start the GC, how much to process on each
    /// cycle, and what other features to automatically enable.
    /// Possible values are:
    /// - adaptive - adapt to maintain the given amount of free heap at all times, even during the GC cycle;
    /// - static -  trigger GC when free heap falls below the threshold;
    /// - aggressive - run GC continuously;
    /// - compact - run GC more frequently and with deeper targets to free up more memory.
    pub heuristics: GCHeuristic,
    pub init_free_threshold: usize,
    /// Pace application allocations to give GC chance to start
    /// and complete before allocation failure is reached.
    pub pacing: bool,
    /// Max delay for pacing application allocations. Larger values
    /// provide more resilience against out of memory, at expense at
    /// hiding the GC latencies in the allocation path. Time is in
    /// milliseconds. Setting it to arbitrarily large value makes
    /// GC effectively stall the threads indefinitely instead of going
    /// to degenerated or Full GC.
    pub pacing_max_delay: usize,
    /// How much of free space to take as non-taxable allocations the GC cycle.
    /// Larger value makes the pacing milder at the beginning of the cycle.
    /// Lower value makes the pacing less uniform during the cyclent. In percent of free space
    pub pacing_cycle_slack: usize,
    pub pacing_idle_slack: usize,
    /// Additional pacing tax surcharge to help unclutter the heap.
    /// Larger values makes the pacing more aggressive. Lower values
    /// risk GC cycles finish with less memory than were available at the beginning of it.
    pub pacing_surcharge: f64,
    
    pub parallel_root_mark_tasks: bool,
}

impl HeapArguments {
    pub fn from_env() -> Self {
        let mut this = Self::default();

        match read_uint_from_env("GC_MIN_REGION_SIZE") {
            Some(size) => this.min_region_size = size,
            None => (),
        }

        match read_uint_from_env("GC_MAX_REGION_SIZE") {
            Some(size) => this.max_region_size = size,
            None => (),
        }

        match read_uint_from_env("GC_REGION_SIZE") {
            Some(size) => this.region_size = size,
            None => (),
        }

        match read_uint_from_env("GC_ELASTIC_TLAB") {
            Some(x) => this.elastic_tlab = x > 0,
            None => (),
        }

        match read_uint_from_env("GC_TARGET_NUM_REGIONS") {
            Some(x) => this.target_num_regions = x,
            None => (),
        }

        match read_uint_from_env("GC_HUMONGOUS_THRESHOLD") {
            Some(x) => this.humongous_threshold = x,
            None => (),
        }

        match read_uint_from_env("GC_MIN_TLAB_SIZE") {
            Some(x) => this.min_tlab_size = x,
            None => (),
        }

        match read_uint_from_env("GC_TLAB_SIZE") {
            Some(x) => {
                this.tlab_size = x;
            }
            None => (),
        }

        match read_uint_from_env("GC_TLAB_WASTE_TARGET_PERCENT") {
            Some(x) => this.tlab_waste_target_percent = x,
            None => (),
        }

        match read_uint_from_env("GC_TLAB_REFILL_WASTE_FRACTION") {
            Some(x) => this.tlab_refill_waste_fraction = x,
            None => (),
        }

        match read_uint_from_env("GC_TLAB_WASTE_INCREMENT") {
            Some(x) => this.tlab_waste_increment = x,
            None => (),
        }

        match read_uint_from_env("GC_MAX_HEAP_SIZE") {
            Some(x) => this.max_heap_size = x,
            None => (),
        }

        match read_uint_from_env("GC_INITIAL_HEAP_SIZE") {
            Some(x) => this.initial_heap_size = x,
            None => (),
        }

        match read_uint_from_env("GC_MIN_HEAP_SIZE") {
            Some(x) => this.min_heap_size = x,
            None => (),
        }

        match read_uint_from_env("GC_MIN_FREE_THRESHOLD") {
            Some(x) => this.min_free_threshold = x,
            None => (),
        }

        match read_uint_from_env("GC_ALLOCATION_THRESHOLD") {
            Some(x) => this.allocation_threshold = x,
            None => (),
        }

        match read_uint_from_env("GC_GUARANTEED_GC_INTERVAL") {
            Some(x) => this.guaranteed_gc_interval = x,
            None => (),
        }

        match read_uint_from_env("GC_CONTROL_INTERVAL_MIN") {
            Some(x) => this.control_interval_min = x,
            None => (),
        }

        match read_uint_from_env("GC_CONTROL_INTERVAL_MAX") {
            Some(x) => this.control_interval_max = x,
            None => (),
        }

        match read_uint_from_env("GC_CONTROL_INTERVAL_ADJUST_PERIOD") {
            Some(x) => this.control_interval_adjust_period = x,
            None => (),
        }

        match read_uint_from_env("GC_UNCOMMIT") {
            Some(x) => this.uncommit = x > 0,
            None => (),
        }

        match read_uint_from_env("GC_UNCOMMIT_DELAY") {
            Some(x) => this.uncommit_delay = x,
            None => (),
        }

        match read_uint_from_env("GC_PARALLEL_REGION_STRIDE") {
            Some(x) => this.parallel_region_stride = x,
            None => (),
        }

        match read_uint_from_env("GC_PARALLEL_THREADS") {
            Some(x) => this.parallel_gc_threads = x,
            None => (),
        }

        match read_uint_from_env("GC_PARALLEL_MARK") {
            Some(x) => this.parallel_mark = x > 0,
            None => (),
        }

        match read_uint_from_env("GC_PARALLEL_SWEEP") {
            Some(x) => this.parallel_sweep = x > 0,
            None => (),
        }

        match read_float_from_env("GC_MIN_RAM_PERCENTAGE") {
            Some(x) => this.min_ram_percentage = x,
            None => (),
        }

        match read_float_from_env("GC_MAX_RAM_PERCENTAGE") {
            Some(x) => this.max_ram_percentage = x,
            None => (),
        }

        match read_float_from_env("GC_INITIAL_RAM_PERCENTAGE") {
            Some(x) => this.initial_ram_percentage = x,
            None => (),
        }

        match read_uint_from_env("GC_MARK_LOOP_STRIDE") {
            Some(x) => this.mark_loop_stride = x,
            None => (),
        }

        match read_uint_from_env("GC_MAX_SATB_BUFFER_FLUSHES") {
            Some(x) => this.max_satb_buffer_flushes = x,
            None => (),
        }

        match read_uint_from_env("GC_MAX_SATB_BUFFER_SIZE") {
            Some(x) => this.max_satb_buffer_size = x,
            None => (),
        }

        match read_uint_from_env("GC_FULL_GC_THRESHOLD") {
            Some(x) => this.full_gc_threshold = x,
            None => (),
        }

        match read_uint_from_env("GC_ALWAYS_FULL") {
            Some(x) => this.always_full = x > 0,
            None => (),
        }

        match read_float_from_env("GC_ADAPTIVE_DECAY_FACTOR") {
            Some(x) => this.adaptive_decay_factor = x,
            None => (),
        }

        match read_uint_from_env("GC_PARALLEL_ROOT_MARKS") {
            Some(x) => this.parallel_root_mark_tasks = x != 0,
            None => (),
        }

        match read_uint_from_env("GC_LEARNING_STEPS") {
            Some(x) => if x == 0 {
                this.learning_steps = 1;
            } else {
                this.learning_steps = x;
            }
            None => ()
        }

        this.heuristics = match std::env::var("GC_HEURISTICS") {
            Ok(x) => match x.to_lowercase().as_str() {
                "adaptive" => GCHeuristic::Adaptive,
                "static" => GCHeuristic::Static,
                _ => todo!("unsupported or unknown heuristic: {}", x),
            },
            _ => GCHeuristic::Adaptive,
        };

        this
    }

    pub fn set_heap_size(&mut self) {
        let phys_mem = get_total_memory();

        if self.max_heap_size == 96 * 1024 * 1024 {
            let mut reasonable_max = ((phys_mem as f64 * self.max_ram_percentage) / 100.0) as usize;
            let reasonable_min = ((phys_mem as f64 * self.min_ram_percentage) / 100.0) as usize;

            if reasonable_min < self.max_heap_size {
                reasonable_max = reasonable_min;
            } else {
                reasonable_max = reasonable_max.max(self.max_heap_size);
            }

            if self.initial_heap_size != 0 {
                reasonable_max = reasonable_max.max(self.initial_heap_size);
            } else if self.min_heap_size != 0 {
                reasonable_max = reasonable_max.max(self.min_heap_size);
            }

            log::info!(target: "gc", " Maximum heap size {}", formatted_size(reasonable_max));
            self.max_heap_size = reasonable_max;
        }

        if self.initial_heap_size == 0 || self.min_heap_size == 0 {
            let mut reasonable_minimum = 5 * 1024 * 1024; // 5MB

            reasonable_minimum = reasonable_minimum.min(self.max_heap_size);

            if self.initial_heap_size == 0 {
                let mut reasonable_initial =
                    ((phys_mem as f64 * self.initial_ram_percentage) / 100.0) as usize;
                reasonable_initial = reasonable_initial
                    .max(reasonable_minimum)
                    .max(self.min_heap_size);
                reasonable_initial = reasonable_initial.min(self.max_heap_size);

                log::info!(target: "gc", " Initial heap size {}", formatted_size(reasonable_initial));
                self.initial_heap_size = reasonable_initial;
            }

            if self.min_heap_size == 0 {
                log::info!(target: "gc", " Minimum heap size {}", formatted_size(reasonable_minimum));
                self.min_heap_size = reasonable_minimum;
            }
        }
    }
}

impl Default for HeapArguments {
    fn default() -> Self {
        Self {
            parallel_root_mark_tasks: true,
            always_full: false,
            min_region_size: virtual_memory::page_size(),
            max_region_size: 32 * 1024 * 1024,
            target_num_regions: 2048,
            humongous_threshold: 100,
            region_size: 0,
            elastic_tlab: false,
            min_free_threshold: 10,
            allocation_threshold: 0,
            guaranteed_gc_interval: 5 * 60 * 1000,
            uncommit: true,
            uncommit_delay: 5 * 60 * 1000,
            control_interval_min: 1,
            control_interval_max: 10,
            control_interval_adjust_period: 1000,
            min_tlab_size: 2 * 1024,
            max_heap_size: 96 * 1024 * 1024,
            min_heap_size: 0,
            min_ram_percentage: 50.0,
            max_ram_percentage: 25.0,
            initial_ram_percentage: 1.5625,
            initial_heap_size: 0,
            parallel_mark: true,
            parallel_sweep: true,
            mark_loop_stride: 1000,
            tlab_refill_waste_fraction: 64,
            tlab_size: 0,
            tlab_waste_increment: 4,
            tlab_waste_target_percent: 1,
            parallel_region_stride: 1024,
            parallel_gc_threads: std::thread::available_parallelism()
                .map(|x| x.get())
                .unwrap_or(2),
            max_satb_buffer_flushes: 5,
            max_satb_buffer_size: 1024,
            full_gc_threshold: 3,
            adaptive_decay_factor: 0.5,
            adaptive_initial_confidence: 1.8,
            adaptive_initial_spike_threshold: 1.8,
            adaptive_sample_frequency_hz: 10,
            adaptive_sample_frequency_size_seconds: 10,
            alloc_spike_factor: 5,
            learning_steps: 5,
            immediate_threshold: 90,
            heuristics: GCHeuristic::Adaptive,
            init_free_threshold: 70,
            pacing: true,
            pacing_cycle_slack: 10,
            pacing_idle_slack: 2,
            pacing_max_delay: 10,
            pacing_surcharge: 1.1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
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
    pub tlab_size: usize,
    pub min_tlab_size: usize,
    pub region_size_log2: usize,
    pub elastic_tlab: bool,
    pub min_free_threshold: usize,
    pub allocation_threshold: usize,
    pub guaranteed_gc_interval: usize,
    pub control_interval_min: usize,
    pub control_interval_max: usize,
    pub control_interval_adjust_period: usize,
    pub uncommit: bool,
    pub uncommit_delay: usize,
    pub target_refills: usize,
    pub parallel_gc_threads: usize,
    pub parallel_region_stride: usize,
    pub parallel_mark: bool,
    pub parallel_sweep: bool,
    pub mark_loop_stride: usize,
    pub max_satb_buffer_flushes: usize,
    pub max_satb_buffer_size: usize,
    pub full_gc_threshold: usize,
    pub always_full: bool,
    pub adaptive_decay_factor: f64,
    pub learning_steps: usize,
    pub immediate_threshold: usize,
    pub adaptive_sample_frequency_hz: usize,
    pub adaptive_sample_frequency_size_seconds: usize,
    pub adaptive_initial_confidence: f64,
    pub adaptive_initial_spike_threshold: f64,
    pub alloc_spike_factor: usize,
    pub init_free_threshold: usize,
    /// Pace application allocations to give GC chance to start
    /// and complete before allocation failure is reached.
    pub pacing: bool,
    /// Max delay for pacing application allocations. Larger values
    /// provide more resilience against out of memory, at expense at
    /// hiding the GC latencies in the allocation path. Time is in
    /// milliseconds. Setting it to arbitrarily large value makes
    /// GC effectively stall the threads indefinitely instead of going
    /// to degenerated or Full GC.
    pub pacing_max_delay: usize,
    /// How much of free space to take as non-taxable allocations the GC cycle.
    /// Larger value makes the pacing milder at the beginning of the cycle.
    /// Lower value makes the pacing less uniform during the cyclent. In percent of free space
    pub pacing_cycle_slack: usize,
    pub pacing_idle_slack: usize,
    /// Additional pacing tax surcharge to help unclutter the heap.
    /// Larger values makes the pacing more aggressive. Lower values
    /// risk GC cycles finish with less memory than were available at the beginning of it.
    pub pacing_surcharge: f64,

    pub parallel_root_mark_tasks: bool,
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
            .field(
                "humongous_threshold_bytes",
                &formatted_size(self.humongous_threshold_bytes),
            )
            .field("max_heap_size", &formatted_size(self.max_heap_size))
            .field("region_size_log2", &self.region_size_log2)
            .finish()
    }
}

impl HeapRegion {
    pub fn end(&self) -> usize {
        self.end as _
    }

    pub unsafe fn new(
        loc: usize,
        index: usize,
        start: usize,
        opts: &HeapOptions,
        is_commited: bool,
    ) -> *mut Self {
        let p = loc as *mut Self;
        p.write(Self {
            visited: false,
            last_sweep_free: 0,
            typ: if is_commited {
                RegionState::EmptyCommited
            } else {
                RegionState::EmptyUncommited
            },
            begin: start as *mut u8,
            index,
            used: 0,
            to_sweep: false,
            object_start_bitmap: HeapBitmap::new(start, opts.region_size_bytes),
            end: (start + opts.region_size_bytes) as *mut u8,
            free_list: FreeList::new(opts),
            largest_free_list_entry: 0,
            empty_time: Instant::now(),
        });

        (*p).recycle();

        p
    }
    pub fn empty_time(&self) -> Instant {
        self.empty_time
    }

    pub fn allocate(&mut self, size: usize) -> *mut u8 {
        unsafe {
            self.make_regular_allocation();
            let (result, sz) = self.free_list.allocate(size);

            if result.is_null() {
                return null_mut();
            }

            if sz > size {
                let remainder = result.add(size);
                let remainder_size = result.add(sz) as usize - remainder as usize;
                self.free_list.add(remainder, remainder_size);
                self.used += sz - remainder_size;
            } else {
                self.used += sz;
            }

            result
        }
    }

    pub fn do_commit(&mut self) {
        PlatformVirtualMemory::commit(self.bottom() as _, self.size());
        heap().increase_commited(self.size());
        self.typ = RegionState::EmptyCommited;
    }

    pub fn do_decommit(&mut self) {
        PlatformVirtualMemory::decommit(self.bottom() as _, self.size());
        PlatformVirtualMemory::dontneed(self.bottom() as _, self.size());

        heap().decrease_commited(self.size());
        self.free_list.clear();
        self.typ = RegionState::EmptyUncommited;
    }

    pub fn make_regular_allocation(&mut self) {
        if self.typ == RegionState::EmptyUncommited {
            self.do_commit();
        }
        if self.typ == RegionState::EmptyCommited {
            unsafe {
                self.free_list.add(self.bottom() as _, self.size());
            }
        }
        self.typ = RegionState::Regular;
    }

    pub fn used(&self) -> usize {
        self.used
    }

    pub fn free(&self) -> usize {
        if self.typ == RegionState::EmptyUncommited || self.typ == RegionState::EmptyCommited {
            return self.size();
        }
        self.free_list.free()
    }
    pub fn peek_free(&self) -> usize {
        self.free_list.peek_free()
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

    pub fn contains(&self, ptr: *mut u8) -> bool {
        ptr >= self.begin && ptr < self.end
    }

    pub fn is_empty(&self) -> bool {
        self.typ == RegionState::EmptyUncommited || self.typ == RegionState::EmptyCommited
    }

    pub fn is_empty_commited(&self) -> bool {
        self.typ == RegionState::EmptyCommited
    }

    pub fn is_alloc_allowed(&self) -> bool {
        self.typ == RegionState::Regular || self.is_empty()
    }

    pub fn is_trash(&self) -> bool {
        self.typ == RegionState::Trash
    }

    pub fn make_humonogous_start(&mut self) {
        if self.typ == RegionState::EmptyUncommited {
            self.do_commit();
        }
        self.typ = RegionState::HumongousStart;
        self.free_list.clear();
    }

    pub fn make_humonogous_cont(&mut self) {
        if self.typ == RegionState::EmptyUncommited {
            self.do_commit();
        }
        self.typ = RegionState::HumongousCont;
        self.free_list.clear();
    }

    pub fn recycle(&mut self) {
        self.free_list.clear();
        if self.typ == RegionState::Trash {
            self.make_empty();
        }
        self.object_start_bitmap.clear();
        self.used = 0;
        self.largest_free_list_entry = self.size();
    }

    pub fn largest_free_list_entry(&self) -> usize {
        self.largest_free_list_entry
    }

    pub fn set_largest_free_list_entry(&mut self, size: usize) {
        self.largest_free_list_entry = size;
    }

    pub fn make_trash(&mut self) {
        self.typ = RegionState::Trash;
    }

    pub fn make_empty(&mut self) {
        self.typ = RegionState::EmptyCommited;
        self.empty_time = Instant::now();
    }

    pub fn is_humongous_start(&self) -> bool {
        self.typ == RegionState::HumongousStart
    }

    pub fn is_humongous_cont(&self) -> bool {
        self.typ == RegionState::HumongousCont
    }

    pub fn is_regular(&self) -> bool {
        self.typ == RegionState::Regular
    }

    pub const MIN_REGION_SIZE: usize = 4 * 1024;
    pub const MIN_NUM_PAGES: usize = 10;
    pub const MAX_REGION_SIZE: usize = 32 * 1024 * 1024;

    /// Setups heap region sizes and thresholds based on input parameters.
    ///
    /// # Notes
    /// - `humongous_threshold` is a percentage of memory in region that could be used for large object, for example:
    /// if you have humongous_threshold set to 50 and your region size is 4KB then largest object that will be allocated
    /// using free-list is 2KB, objects larger than that get a separate humongous region(s).
    pub fn setup_sizes(args: &HeapArguments) -> HeapOptions {
        let mut opts = HeapOptions::default();

        opts.parallel_mark = args.parallel_mark;
        opts.parallel_sweep = args.parallel_sweep;
        opts.target_refills = 100 / (2 * args.tlab_waste_target_percent);
        opts.target_refills = opts.target_refills.max(2);
        opts.tlab_size = args.tlab_size;
        opts.uncommit = args.uncommit;
        opts.parallel_gc_threads = args.parallel_gc_threads;
        opts.parallel_region_stride = args.parallel_region_stride;
        opts.allocation_threshold = args.allocation_threshold;
        opts.min_free_threshold = args.min_free_threshold;
        opts.control_interval_adjust_period = args.control_interval_adjust_period;
        opts.control_interval_max = args.control_interval_max;
        opts.control_interval_min = args.control_interval_min;
        opts.uncommit_delay = args.uncommit_delay;
        opts.guaranteed_gc_interval = args.guaranteed_gc_interval;
        opts.mark_loop_stride = args.mark_loop_stride.max(32);
        opts.max_satb_buffer_flushes = args.max_satb_buffer_flushes;
        opts.max_satb_buffer_size = args.max_satb_buffer_size;
        opts.full_gc_threshold = args.full_gc_threshold;
        opts.always_full = args.always_full;
        opts.adaptive_decay_factor = args.adaptive_decay_factor;
        opts.learning_steps = args.learning_steps;
        opts.immediate_threshold = args.immediate_threshold;
        opts.adaptive_sample_frequency_hz = args.adaptive_sample_frequency_hz;
        opts.adaptive_sample_frequency_size_seconds = args.adaptive_sample_frequency_size_seconds;
        opts.adaptive_initial_confidence = args.adaptive_initial_confidence;
        opts.adaptive_initial_spike_threshold = args.adaptive_initial_spike_threshold;
        opts.alloc_spike_factor = args.alloc_spike_factor;
        opts.init_free_threshold = args.init_free_threshold;
        opts.pacing = args.pacing;
        opts.pacing_cycle_slack = args.pacing_cycle_slack;
        opts.pacing_idle_slack = args.pacing_idle_slack;
        opts.pacing_max_delay = args.pacing_max_delay;
        opts.pacing_surcharge = args.pacing_surcharge;
        opts.parallel_root_mark_tasks = args.parallel_root_mark_tasks;

        let min_region_size = if args.min_region_size < Self::MIN_REGION_SIZE {
            Self::MIN_REGION_SIZE
        } else {
            args.min_region_size
        };

        let target_num_regions = if args.target_num_regions == 0 {
            2048
        } else {
            args.target_num_regions
        };
        let max_region_size =
            if args.max_region_size == 0 || args.max_region_size < args.min_region_size {
                Self::MAX_REGION_SIZE
            } else {
                args.max_region_size
            };

        let mut max_heap_size = args.max_heap_size;

        if min_region_size > max_heap_size / Self::MIN_NUM_PAGES {
            panic!("Max heap size ({}) is too low to afford the minimum number of regions ({}) of minimum region size ({})",
                formatted_size(max_heap_size) ,Self::MIN_NUM_PAGES,formatted_size(min_region_size)
            );
        }

        let mut region_size = if args.region_size != 0 {
            args.region_size
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
        let humongous_threshold = if args.humongous_threshold == 0 {
            100
        } else {
            args.humongous_threshold
        };
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
        opts.min_tlab_size = if args.min_tlab_size < 2 * 1024 {
            2 * 1024
        } else {
            args.min_tlab_size
        };
        opts.max_tlab_size = opts.min_tlab_size.max(opts.max_tlab_size);
        opts.elastic_tlab = args.elastic_tlab;
        if opts.tlab_size != 0 {
            opts.tlab_size = opts.tlab_size.min(opts.humongous_threshold_bytes);
        }

        if opts.parallel_region_stride == 0 && opts.parallel_gc_threads != 0
            || opts.parallel_region_stride == 1024
        {
            opts.parallel_region_stride = opts.region_count / opts.parallel_gc_threads;
        }

        log::info!(target: "gc", "Region sizes setup complete");
        log::info!(target: "gc", "- Max heap size: {}", formatted_size(opts.max_heap_size));
        log::info!(target: "gc", "- Region count: {}", opts.region_count);
        log::info!(target: "gc", "- Region size: {}", formatted_size(opts.region_size_bytes));
        log::info!(target: "gc", "- Humongous threshold: {}", formatted_size(opts.humongous_threshold_bytes));
        log::info!(target: "gc", "- Max TLAB size: {}", formatted_size(opts.max_tlab_size));
        log::info!(target: "gc", "- TLAB size: {}", formatted_size(opts.tlab_size));
        opts
    }
}
