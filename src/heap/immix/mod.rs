pub mod block;

use std::mem::{size_of, MaybeUninit};

use parking_lot::{lock_api::RawMutex, RawMutex as Lock};

use super::{virtual_memory::{self, VirtualMemory, page_size}, align_usize, align_down, bitmap::CHeapBitmap};
use block::Block;

pub struct ImmixHeap {
    pub(crate) lock: Lock,
    mem: Box<VirtualMemory>,
    blocks_storage: Box<VirtualMemory>,
    blocks: Vec<*mut Block>,
    line_bitmap: CHeapBitmap,
    mark_bitmap: CHeapBitmap,
    live_bitmap: CHeapBitmap
}

impl ImmixHeap {
    pub fn new(_args: ImmixArguments) -> &'static mut Self {

        todo!()
    }
}


struct HeapRef(&'static mut ImmixHeap);

unsafe impl Send for HeapRef {}
unsafe impl Sync for HeapRef {}

static mut HEAP: MaybeUninit<HeapRef> = MaybeUninit::uninit();

pub fn heap() -> &'static mut ImmixHeap {
    unsafe { HEAP.assume_init_mut().0 }
}

pub struct ImmixArguments {
    pub max_heap_size: usize,
    pub line_size: usize,
    pub block_size: usize,
    pub min_block_size: usize,
    pub max_block_size: usize,
    pub min_line_size: usize,
    pub max_line_size: usize,
    pub humongous_threshold: usize,
    pub target_num_blocks: usize,
    pub target_line_count: usize,
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
}

impl Default for ImmixArguments {
    fn default() -> Self {
        Self {
            min_block_size: 8 * 1024,
            max_block_size: 64 * 1024,
            min_line_size: 128,
            max_line_size: 1 * 1024,
            block_size: 0,
            target_num_blocks: 2048,
            humongous_threshold: 50,
            line_size: 0,
            target_line_count: 128,
            min_free_threshold: 10,
            allocation_threshold: 10,
            guaranteed_gc_interval: 5 * 60 * 1000,
            uncommit: true,
            uncommit_delay: 5 * 60 * 1000,
            control_interval_min: 1,
            control_interval_max: 10,
            control_interval_adjust_period: 1000,
            max_heap_size: 96 * 1024 * 1024,
            parallel_region_stride: 1024,
            parallel_gc_threads: 4,
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub struct ImmixOptions {
    pub max_heap_size: usize,
    pub line_size: usize,
    pub block_size: usize,
    pub min_block_size: usize,
    pub max_block_size: usize,
    pub min_line_size: usize,
    pub max_line_size: usize,
    pub humongous_threshold: usize,
    pub target_num_blocks: usize,
    pub target_line_count: usize,
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

    pub block_size_bytes: usize,
    pub block_size_words: usize,
    pub line_size_bytes: usize,
    pub line_size_words: usize,
    pub line_size_bytes_mask: usize,
    pub line_size_bytes_shift: usize,
    pub line_size_log2: usize,
    pub line_count: usize,
    pub block_count: usize,
    pub block_size_bytes_shift: usize,
    pub block_size_bytes_mask: usize,
    pub block_size_words_shift: usize,
    pub block_size_words_mask: usize,
    pub humongous_threshold_words: usize,
    pub humongous_threshold_bytes: usize,
    pub block_size_log2: usize,
}

impl ImmixOptions {
    const MIN_BLOCK_SIZE: usize = 8 * 1024;
    const MAX_BLOCK_SIZE: usize = 128 * 1024;
    const MIN_LINE_SIZE: usize = 128;
    const MAX_LINE_SIZE: usize = 2 * 1024;

    pub fn setup_sizes(args: ImmixArguments) -> Self {
        let mut opts = Self::default();

        opts.uncommit = args.uncommit;
        opts.uncommit_delay = args.uncommit_delay;
        opts.parallel_gc_threads = args.parallel_gc_threads;
        opts.parallel_region_stride = args.parallel_region_stride;

        let min_block_size = if args.min_block_size < Self::MIN_BLOCK_SIZE {
            Self::MIN_BLOCK_SIZE
        } else {
            args.min_block_size
        };

        let target_num_blocks = if args.target_num_blocks == 0 {
            2048
        } else {
            args.target_num_blocks
        };

        let max_block_size = if args.max_block_size > Self::MAX_BLOCK_SIZE
            || args.max_block_size == 0
            || args.max_block_size < opts.min_block_size
        {
            Self::MAX_BLOCK_SIZE
        } else {
            args.max_block_size
        };

        let mut max_heap_size = args.max_heap_size;

        

        let mut block_size = if args.block_size != 0 {
            args.block_size
        } else {
            let mut block_size = max_heap_size / target_num_blocks;
            block_size = block_size.max(min_block_size);
            block_size = max_block_size.min(block_size);

            block_size
        };

        let min_line_size = if args.min_line_size < Self::MIN_LINE_SIZE {
            Self::MIN_LINE_SIZE
        } else {
            args.min_line_size
        };

        let max_line_size = if args.max_line_size > Self::MAX_LINE_SIZE
            || args.max_line_size == 0
            || args.max_line_size < opts.min_line_size
        {
            Self::MAX_LINE_SIZE
        } else {
            args.max_line_size
        };

        

        block_size = align_usize(block_size, page_size());
        max_heap_size = align_usize(max_heap_size, block_size);
        
        let block_size_log2 = (block_size as f64).log2() as usize;
        let block_size = 1 << block_size_log2;
        max_heap_size = align_usize(max_heap_size, block_size);
        let mut line_size = if args.line_size != 0 {
            args.line_size
        } else {
            let mut line_size = block_size / args.target_line_count;
            line_size = line_size.max(min_line_size);
            line_size = max_line_size.min(line_size);

            line_size
        };

        line_size = align_usize(line_size, Self::MIN_LINE_SIZE);
        let line_size_log = (line_size as f64).log2() as usize;
        line_size = 1 << line_size_log;
        opts.block_count = max_heap_size / block_size;
        opts.block_size_bytes = block_size;
        opts.block_size_words = block_size / size_of::<usize>();
        opts.block_size_bytes_mask = block_size - 1;
        opts.block_size_words_mask = block_size - 1;
        opts.block_size_bytes_shift = block_size_log2;
        opts.block_size_words_shift = block_size_log2 - 3;
        opts.block_size_log2 = block_size_log2;
        opts.line_size_bytes = line_size;
        opts.line_count = block_size / line_size;
        opts.line_size_words = line_size / size_of::<usize>();
        opts.line_size_log2 = line_size_log;
        opts.line_size_bytes_mask = line_size -1;
        opts.line_size_bytes_shift = line_size_log;

        let humongous_threshold = if args.humongous_threshold == 0 {
            100 
        } else {
            args.humongous_threshold
        };

        opts.humongous_threshold_words = opts.block_size_words * humongous_threshold / 100;
        opts.humongous_threshold_words = align_down(opts.humongous_threshold_words, 8);
        opts.humongous_threshold_bytes = opts.humongous_threshold_words * size_of::<usize>();
        opts.max_heap_size = max_heap_size;

        opts
    }
}
