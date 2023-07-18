use crate::{
    block_allocator::BlockAllocator,
    constants::{
        ALLOCATION_ALIGNMENT, BLOCK_TOTAL_SIZE, EARLY_GROWTH_RATE, EARLY_GROW_THRESHOLD,
        GROWTH_RATE, LINE_COUNT, LINE_METADATA_SIZE, LINE_SIZE, LINE_SIZE_BITS, MAX_HEAP_SIZE,
        MIN_HEAP_SIZE, SPACE_USED_PER_BLOCK, UNLIMITED_HEAP_SIZE, WORDS_IN_BLOCK, WORD_SIZE,
    },
    large_allocator::LargeAllocator,
    mark::Marker,
    memory_map::*,
    meta::block::{recycle_block, BlockMeta},
    safepoint::{self, install_signal_handlers},
    thread::{Thread, ThreadNode},
    utils::{bytemap::Bytemap, formatted_size, get_total_memory},
};
use parking_lot::lock_api::RawMutex;
use std::{
    mem::{size_of, MaybeUninit},
    ptr::{addr_of_mut, null_mut},
    sync::atomic::AtomicBool,
};

pub struct Heap {
    pub(crate) lock: parking_lot::RawMutex,
    pub(crate) thread_mod_lock: parking_lot::RawMutex,
    pub(crate) block_meta_start: *mut usize,
    pub(crate) block_meta_end: *mut usize,
    pub(crate) line_meta_start: *mut usize,
    pub(crate) line_meta_end: *mut usize,
    pub(crate) heap_start: *mut usize,
    pub(crate) heap_end: *mut usize,
    pub(crate) heap_size: usize,
    pub(crate) max_heap_size: usize,
    pub(crate) block_count: u32,
    pub(crate) max_block_count: u32,
    pub(crate) bytemap: *mut Bytemap,
    pub(crate) block_allocator: BlockAllocator,
    pub(crate) large_allocator: LargeAllocator,
    pub(crate) threads: *mut ThreadNode,
}

static mut HEAP: MaybeUninit<Heap> = MaybeUninit::uninit();
static INIT: AtomicBool = AtomicBool::new(false);
impl Heap {
    pub fn get_line_meta_for_word(&self, word: *const usize) -> *mut u8 {
        let line_global_index = (word as usize - self.heap_start as usize) >> LINE_SIZE_BITS;
        unsafe { self.line_meta_start.cast::<u8>().add(line_global_index) }
    }

    pub fn is_word_in_heap(&self, word: *const usize) -> bool {
        word >= self.heap_start && word < self.heap_end
    }

    pub fn is_growing_possible(&self, increment: usize) -> bool {
        self.block_count + increment as u32 <= self.max_block_count
    }

    pub fn get_memory_limit() -> usize {
        let msize = get_total_memory();
        if msize > MAX_HEAP_SIZE {
            MAX_HEAP_SIZE
        } else {
            msize
        }
    }

    unsafe fn map_and_align(memory_limit: usize, alignment_size: usize) -> *mut usize {
        let mut heap_start = memory_map(memory_limit);
        let alignment_mask = !(alignment_size - 1);

        if (heap_start as usize & alignment_mask) != heap_start as usize {
            let previous_block = (heap_start as usize & alignment_mask) as *mut usize;
            heap_start = previous_block.add(alignment_size / WORD_SIZE).cast();
        }

        heap_start.cast()
    }

    pub fn init(mut min_heap_size: usize, mut max_heap_size: usize) {
        unsafe {
            if INIT.load(std::sync::atomic::Ordering::Relaxed) {
                panic!("Heap already initialized");
            }
            install_signal_handlers();
            let mut ptr = null_mut();
            safepoint::safepoint_init(&mut ptr);
            safepoint::SAFEPOINT_PAGE.store(ptr as _, std::sync::atomic::Ordering::Relaxed);
            let limit = Self::get_memory_limit();
            let heap = HEAP.as_mut_ptr();

            if max_heap_size < MIN_HEAP_SIZE {
                panic!(
                    "GC_MAXIMUM_HEAP_SIZE too small to initialize heap, minimum required size: {}M",
                    MIN_HEAP_SIZE / 1024 / 1024
                )
            }

            if min_heap_size > limit {
                panic!(
                    "GC_MINIMUM_HEAP_SIZE too large to initialize heap, maximum allowed size: {}G",
                    limit / 1024 / 1024 / 1024
                )
            }

            if max_heap_size < min_heap_size {
                panic!("GC_MAXIMUM_HEAP_SIZE smaller than GC_MINIMUM_HEAP_SIZE")
            }

            if max_heap_size < MIN_HEAP_SIZE {
                min_heap_size = MIN_HEAP_SIZE;
            }

            if max_heap_size == UNLIMITED_HEAP_SIZE {
                max_heap_size = limit;
            }

            let max_number_of_blocks = max_heap_size / SPACE_USED_PER_BLOCK;
            let initial_block_count = min_heap_size / SPACE_USED_PER_BLOCK;

            (*heap).max_heap_size = max_heap_size;
            (*heap).block_count = initial_block_count as _;
            (*heap).max_block_count = max_number_of_blocks as _;

            let block_meta_space_size = max_number_of_blocks * size_of::<BlockMeta>();

            let block_meta_start = Self::map_and_align(block_meta_space_size, WORD_SIZE);

            (*heap).block_meta_start = block_meta_start;
            (*heap).block_meta_end =
                block_meta_start.add(initial_block_count * size_of::<BlockMeta>() / WORD_SIZE);

            let line_meta_space_size = max_number_of_blocks * LINE_COUNT * LINE_METADATA_SIZE;

            let line_meta_start = Self::map_and_align(line_meta_space_size, LINE_SIZE);

            (*heap).line_meta_start = line_meta_start;
            (*heap).line_meta_end = line_meta_start
                .add(initial_block_count * LINE_COUNT * LINE_METADATA_SIZE / WORD_SIZE);

            let bytemap_space_size = max_heap_size / ALLOCATION_ALIGNMENT + size_of::<Bytemap>();

            let bytemap =
                Self::map_and_align(bytemap_space_size, ALLOCATION_ALIGNMENT).cast::<Bytemap>();

            (*heap).bytemap = bytemap;

            let heap_start = Self::map_and_align(max_heap_size, BLOCK_TOTAL_SIZE);

            (*heap).heap_start = heap_start;
            (*heap).heap_end = heap_start.add(min_heap_size / WORD_SIZE);
            log::info!(target: "gc", "Heap initialized: {:p}->{:p} ({}, max {})", (*heap).heap_start, (*heap).heap_end, formatted_size(min_heap_size), formatted_size(max_heap_size));
            (*heap).heap_size = min_heap_size;

            #[cfg(windows)]
            {
                let commit_status = memory_commit(block_meta_start.cast(), block_meta_space_size)
                    && memory_commit(line_meta_start.cast(), line_meta_space_size)
                    && memory_commit(bytemap.cast(), bytemap_space_size)
                    && memory_commit(heap_start.cast(), min_heap_size);

                if !commit_status {
                    panic!("Failed to commit memory");
                }
            }

            let allocator = BlockAllocator::new((*heap).block_meta_start, initial_block_count as _);
            (*heap).block_allocator = allocator;
            Bytemap::init((*heap).bytemap, heap_start.cast(), max_heap_size);
            (*heap).lock = parking_lot::RawMutex::INIT;
            (*heap).thread_mod_lock = parking_lot::RawMutex::INIT;

            let block_allocator = addr_of_mut!((*heap).block_allocator);

            (*heap).large_allocator = LargeAllocator::new(
                block_allocator,
                (*heap).bytemap,
                (*heap).block_meta_start,
                (*heap).heap_start,
            );
            INIT.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn get() -> &'static mut Heap {
        unsafe {
            if !INIT.load(std::sync::atomic::Ordering::Relaxed) {
                panic!("Heap not initialized");
            }
            HEAP.assume_init_mut()
        }
    }

    pub fn collect(&mut self) {
        unsafe {
            let t = Thread::current();
            if !safepoint::enter(t) {
                return;
            }
            self.lock.lock();
            self.thread_mod_lock.lock();

            let mut marker = Marker::new(self);

            marker.mark_roots();

            self.recycle();
            safepoint::leave();
            self.thread_mod_lock.unlock();
            self.lock.unlock();
        }
    }

    unsafe fn recycle(&mut self) {
        self.block_allocator.clear();

        let mut current = self.block_meta_start.cast::<BlockMeta>();
        let mut current_block_start = self.heap_start;
        let mut lines_meta = self.line_meta_start.cast::<u8>();
        let end = self.block_meta_end.cast::<BlockMeta>();

        while current < end {
            let mut size = 1;
            if (*current).is_simple_block() {
                recycle_block(
                    &mut self.block_allocator,
                    self.bytemap,
                    current,
                    current_block_start,
                    lines_meta,
                );
            } else if (*current).is_humongous_start() {
                size = (*current).humongous_size() as usize;
                self.large_allocator.sweep(current, current_block_start);
            } else {
                assert!((*current).is_free());
                self.block_allocator.add_free_blocks(current, size);
            }

            current = current.add(size);
            current_block_start = current_block_start.add(size * WORDS_IN_BLOCK);
            lines_meta = lines_meta.add(size * LINE_COUNT);
        }

        if self.should_grow() {
            let growth = if self.heap_size < EARLY_GROW_THRESHOLD {
                EARLY_GROWTH_RATE
            } else {
                GROWTH_RATE
            };

            let blocks = (self.block_count as f64 * (growth - 1.0)) as usize;

            if self.is_growing_possible(blocks) {
                self.grow(blocks)
            } else {
                let remaining_growth = self.max_block_count as isize - self.block_count as isize;
                if remaining_growth > 0 {
                    self.grow(remaining_growth as usize);
                }
            }
        }

        self.block_allocator.sweep_done();

        let mut node = self.threads;

        while !node.is_null() {
            if !(*node)
                .thread
                .allocator
                .assume_init_mut()
                .can_init_cursors()
            {
                eprintln!("Out of memory: tried to re-init cursors");
                std::process::abort();
            }

            (*node).thread.allocator.assume_init_mut().init_cursors();
            node = (*node).next;
        }
    }

    pub(crate) fn grow(&mut self, increment: usize) {
        self.block_allocator.acquire();
        if !self.is_growing_possible(increment) {
            eprintln!("Out of memory: tried to grow heap");
            std::process::abort();
        }

        let increment_in_bytes = increment * SPACE_USED_PER_BLOCK;

        log::info!(
            target: "gc",
            "Growing heap by {} to {}",
            formatted_size(increment_in_bytes),
            formatted_size(self.heap_size + increment_in_bytes)
        );

        let heap_end = self.heap_end;
        self.heap_size += increment_in_bytes;
        let block_meta_end = self.block_meta_end;
        unsafe {
            self.heap_end = heap_end.add(increment * WORDS_IN_BLOCK);
            self.block_meta_end = block_meta_end.cast::<BlockMeta>().add(increment).cast();
            self.line_meta_end = self
                .line_meta_end
                .add(increment * LINE_COUNT * LINE_METADATA_SIZE / WORD_SIZE);

            #[cfg(windows)]
            {
                // Windows does not allow for over-committing, therefore we commit the
                // next chunk of memory when growing the heap. Without this, the process
                // might take over all available memory leading to OutOfMemory errors for
                // other processes. Also when using UNLIMITED heap size it might try to
                // commit more memory than is available.
                if !memory_Commit(heap_end, increment_in_bytes) {
                    eprintln!("Out of memory: failed to commit memory");
                    std::process::abort();
                }
            }

            self.block_allocator
                .add_free_blocks(block_meta_end.cast(), increment);

            self.block_count += increment as u32;

            self.block_allocator.sweep_done();
            self.block_allocator.release();
        }
    }

    pub(crate) fn should_grow(&self) -> bool {
        let free_block_count = self.block_allocator.free_block_count;
        let block_count = self.block_count;
        let recycled_block_count = self.block_allocator.recycled_blocks_count;

        let unavailable_block_count = block_count - (free_block_count + recycled_block_count);

        log::info!(target: "gc", "\n\nBlock count: {}\nUnavailable: {}\nFree: {}\nRecycled: {}", block_count, unavailable_block_count, free_block_count, recycled_block_count);

        free_block_count * 2 < block_count || (4 * unavailable_block_count) > block_count
    }
}
