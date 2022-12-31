use std::{
    mem::{size_of, MaybeUninit},
    ptr::null_mut,
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};

use crate::{
    formatted_size,
    heap::{root_processor::RootTask, safepoint::SafepointSynchronize},
    sync::{suspendible_thread_set::SuspendibleThreadSet, worker_threads::WorkerThreads},
    system::object::HeapObjectHeader,
    system::traits::{Visitor, WeakProcessor},
    thread::{safepoint_scope, threads},
    utils::is_aligned,
};

use super::{
    align_up, align_usize,
    bitmap::HeapBitmap,
    card_table::{age_card_visitor, CardTable},
    concurrent_thread::ConcurrentGCThread,
    controller::ControlThread,
    free_set::RegionFreeSet,
    heuristics::{adaptive::AdaptiveHeuristics, static_::StaticHeuristics, Heuristics},
    mark::SlotVisitor,
    marking_context::MarkingContext,
    memory_region::MemoryRegion,
    region::{HeapArguments, HeapOptions, HeapRegion},
    root_processor::{Root, RootSet, SimpleRoot},
    safepoint,
    shared_vars::{SharedEnumFlag, SharedFlag},
    thread::Thread,
    virtual_memory::{self, page_size, PlatformVirtualMemory, VirtualMemory, VirtualMemoryImpl},
    AllocRequest, GCHeuristic,
};
use parking_lot::{lock_api::RawMutex, RawMutex as Lock};
use scoped_thread_pool::Pool;

pub trait GlobalRoot {
    fn walk(&self, visitor: &mut dyn Visitor);
}

#[repr(C)]
pub struct Heap {
    pub(crate) lock: Lock,
    pub(crate) is_concurrent_mark_in_progress: SharedFlag,
    marking_context: &'static mut MarkingContext,
    mem: Box<VirtualMemory>,
    regions_storage: Box<VirtualMemory>,
    card_table: CardTable,
    regions: Vec<*mut HeapRegion>,
    opts: Box<HeapOptions>,
    free_set: RegionFreeSet,
    initial_size: usize,
    minimum_size: usize,
    used: AtomicUsize,
    commited: AtomicUsize,
    bytes_allocated_since_gc_start: AtomicUsize,
    last_cycle_end: Instant,
    controller_thread: Option<&'static mut ControlThread>,
    progress_last_gc: SharedFlag,
    object_start_bitmap: HeapBitmap<16>,
    heuristics: Box<dyn Heuristics>,
    root_set: RootSet,
    scoped_pool: Pool,
    cancelled_gc: SharedEnumFlag,
}

pub const GC_CANCELLABLE: u8 = 0;
pub const GC_CANCELLED: u8 = 1;
pub const GC_NOT_CANCELLED: u8 = 2;

impl Heap {
    pub unsafe fn options_mut(&mut self) -> &mut HeapOptions {
        &mut self.opts
    }

    pub(crate) fn free_set(&self) -> &RegionFreeSet {
        &self.free_set
    }

    pub(crate) fn free_set_mut(&mut self) -> &mut RegionFreeSet {
        &mut self.free_set
    }

    pub fn options(&self) -> &HeapOptions {
        &self.opts
    }

    pub fn card_table(&self) -> &CardTable {
        &self.card_table
    }

    pub fn is_concurrent_mark_in_progress(&self) -> bool {
        self.is_concurrent_mark_in_progress.is_set()
    }

    pub(crate) fn set_concurrent_mark_in_progress(&self, value: bool) {
        if value {
            self.is_concurrent_mark_in_progress.set();
        } else {
            self.is_concurrent_mark_in_progress.unset();
        }
    }

    pub(crate) fn root_set(&self) -> &RootSet {
        &self.root_set
    }

    pub fn new(mut args: HeapArguments) -> &'static mut Self {
        assert!(!INIT.fetch_or(true, Ordering::AcqRel));
        safepoint::init();
        SuspendibleThreadSet::init();
        args.set_heap_size();
        let heuristics = args.heuristics;
        let mut opts = HeapRegion::setup_sizes(&args);

        let init_byte_size = args.initial_heap_size;
        let min_byte_size = args.min_heap_size;

        let mut num_commited_regions = init_byte_size / opts.region_size_bytes;
        num_commited_regions = num_commited_regions.min(opts.region_count);

        let initial_size = num_commited_regions * opts.region_size_bytes;

        let num_min_regions = min_byte_size / opts.region_size_bytes;
        let minimum_size = num_min_regions * opts.region_size_bytes;

        let mem = VirtualMemory::<PlatformVirtualMemory>::allocate_aligned(
            opts.max_heap_size,
            opts.region_size_bytes,
            false,
            "heap",
        )
        .unwrap();

        unsafe {
            let uncommit_start = mem.address().add(initial_size);
            let uncommit_size = mem.size() - initial_size;
            if uncommit_size != 0
                && is_aligned(
                    uncommit_size,
                    VirtualMemory::<PlatformVirtualMemory>::page_size(),
                    0,
                )
            {
                PlatformVirtualMemory::decommit(uncommit_start, uncommit_size);
            }
        }
        //PlatformVirtualMemory::commit(mem.address(), initial_size);

        let mark_ctx = Box::leak(Box::new(MarkingContext::new(
            MemoryRegion::new(mem.start() as _, mem.size()),
            opts.parallel_gc_threads.max(1),
        )));

        let size = align_usize(opts.region_count * size_of::<HeapRegion>(), page_size());

        let regions_mem =
            VirtualMemory::allocate_aligned(size, page_size(), false, "heap regions").unwrap();
        let free_set = RegionFreeSet::new(&opts);

        let heuristics = match heuristics {
            GCHeuristic::Adaptive => AdaptiveHeuristics::new(&opts),
            GCHeuristic::Static => StaticHeuristics::new(&mut opts),
        };
        let card_table = CardTable::new(mem.address(), mem.size());
        let this = Box::leak(Box::new(Self {
            card_table,
            object_start_bitmap: HeapBitmap::new(mem.start(), mem.size()),
            regions: vec![null_mut(); opts.region_count],
            regions_storage: Box::new(regions_mem),
            mem: Box::new(mem),
            heuristics,
            is_concurrent_mark_in_progress: SharedFlag::new(),
            opts: Box::new(opts),
            initial_size,
            minimum_size,
            commited: AtomicUsize::new(initial_size),
            lock: Lock::INIT,
            free_set,
            used: AtomicUsize::new(0),
            bytes_allocated_since_gc_start: AtomicUsize::new(0),
            last_cycle_end: Instant::now(),
            controller_thread: None,
            progress_last_gc: SharedFlag::new(),
            root_set: RootSet::new(),
            scoped_pool: Pool::new(opts.parallel_gc_threads),
            marking_context: mark_ctx,
            cancelled_gc: SharedEnumFlag::new(),
        }));
        let ptr = this as *mut Heap;
        for i in 0..opts.region_count {
            let start = this.mem.start() + this.opts.region_size_bytes * i;
            let loc = this.regions_storage.start() + i * size_of::<HeapRegion>();
            let is_commited = i < num_commited_regions;

            unsafe {
                let region = HeapRegion::new(loc, i, start, &this.opts, is_commited);
                this.regions[i] = region;
            }
        }

        this.free_set.set_heap(ptr);
        this.free_set.rebuild();

        unsafe {
            HEAP = MaybeUninit::new(HeapRef(&mut *(this as *mut Self)));

            heap().controller_thread = Some(ControlThread::new());
            INIT.store(true, Ordering::SeqCst);
        }
        for i in 0..this.marking_context.mark_queues().nworkers() {
            let visitor = this.marking_context.mark_queues().visitor(i);
            unsafe {
                (*visitor).set_mark_ctx(this.marking_context as *const MarkingContext);
            }
        }
        this
    }

    pub fn min_capacity(&self) -> usize {
        self.minimum_size
    }

    pub(crate) fn entry_uncommit(&mut self, shrink_before: Instant, shrink_until: usize) {
        // Application allocates from the beginning of the heap.
        // It is more efficient to uncommit from the end, so that applications
        // could enjoy the near committed regions.

        let mut count = 0;
        for i in (0..self.num_regions()).rev() {
            let r = self.get_region(i);
            unsafe {
                if (*r).is_empty_commited() && (*r).empty_time() < shrink_before {
                    self.lock.lock();
                    if (*r).is_empty_commited() {
                        if self.get_commited() < shrink_until + self.options().region_size_bytes {
                            self.lock.unlock();
                            break;
                        }
                    }

                    (*r).do_decommit();
                    count += 1;
                    self.lock.unlock();
                }

                std::hint::spin_loop();
            }
        }
        if count > 0 {
            self.controller_thread().notify_heap_changed();
        }
    }

    pub(crate) fn increase_commited(&self, bytes: usize) {
        self.commited.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn decrease_commited(&self, bytes: usize) {
        self.commited.fetch_sub(bytes, Ordering::Relaxed);
    }

    pub fn get_commited(&self) -> usize {
        self.commited.load(Ordering::Relaxed)
    }

    pub(crate) fn controller_thread(&self) -> &ControlThread {
        self.controller_thread.as_ref().unwrap()
    }

    pub fn object_start_bitmap(&self) -> &HeapBitmap<16> {
        &self.object_start_bitmap
    }

    pub unsafe fn object_start_bitmap_mut(&mut self) -> &mut HeapBitmap<16> {
        &mut self.object_start_bitmap
    }

    pub(crate) fn increase_used(&self, bytes: usize) {
        self.used.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn decrease_used(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }

    pub(crate) fn set_used(&self, bytes: usize) {
        self.used.store(bytes, Ordering::Relaxed);
    }

    pub(crate) fn increase_allocated(&self, bytes: usize) {
        self.bytes_allocated_since_gc_start
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn decrease_allocated(&self, bytes: usize) {
        self.bytes_allocated_since_gc_start
            .fetch_sub(bytes, Ordering::Relaxed);
    }

    pub(crate) fn set_allocated(&self, bytes: usize) {
        self.bytes_allocated_since_gc_start
            .store(bytes, Ordering::Relaxed);
    }

    pub fn request_gc(&self) {
        self.controller_thread().handle_requested_gc();
    }

    pub(crate) fn notify_mutator_alloc(&self, bytes: usize, waste: bool) {
        if !waste {
            //self.increase_used(bytes);
        }

        self.increase_allocated(bytes);
    }

    pub fn bytes_allocated_since_gc_start(&self) -> usize {
        self.bytes_allocated_since_gc_start.load(Ordering::Relaxed)
    }

    pub(crate) fn notify_gc_progress(&self) {
        self.progress_last_gc.set();
    }

    pub(crate) fn notify_no_gc_progress(&self) {
        self.progress_last_gc.unset();
    }

    pub fn region_index(&self, object: *mut u8) -> usize {
        let offset = object as usize - self.mem.start();
        offset >> self.opts.region_size_bytes_shift
    }

    pub fn mem_start(&self) -> usize {
        self.mem.start()
    }

    pub fn mem_end(&self) -> usize {
        self.mem.end()
    }

    pub fn num_regions(&self) -> usize {
        self.opts.region_count
    }

    pub(crate) fn get_region(&self, index: usize) -> *mut HeapRegion {
        self.regions[index]
    }

    pub fn add_root(&mut self, root: impl Root + 'static) {
        self.lock.lock();
        self.root_set.add_root(Arc::new(root));
        unsafe {
            self.lock.unlock();
        }
    }

    pub(crate) fn for_each_visitor(&self, mut cb: impl FnMut(*mut dyn Visitor)) {
        self.marking_context()
            .mark_queues()
            .visitors()
            .iter()
            .for_each(|vis| cb(*vis));

        cb(heap().marking_context);
    }

    pub fn process_cards(&mut self, clear: bool) {
        if clear {
            self.card_table
                .clear_card_range(self.mem.start() as _, self.mem.end() as _);
        } else {
            // Age cards: if a card is dirty, it is aged by one. If it is not dirty, it is cleared.
            self.card_table.modify_cards_atomic(
                self.mem.start() as _,
                self.mem.end() as _,
                age_card_visitor,
                |_, _, _| {},
            );
        }
    }

    pub fn max_capacity(&self) -> usize {
        self.regions.len() * self.options().region_size_bytes
    }

    pub unsafe fn max_tlab_alloc(&self) -> usize {
        if self.options().elastic_tlab {
            self.options().max_tlab_size
        } else {
            self.free_set
                .unsafe_peek_free()
                .min(self.options().max_tlab_size)
        }
    }

    pub(crate) fn heuristics(&self) -> &dyn Heuristics {
        &*self.heuristics
    }

    pub(crate) fn heuristics_mut(&mut self) -> &mut dyn Heuristics {
        &mut *self.heuristics
    }

    fn allocate_memory_under_lock(
        &mut self,
        req: &mut AllocRequest,
        in_new_region: &mut bool,
    ) -> *mut u8 {
        self.lock.lock();
        let mem = self.free_set.allocate(req, in_new_region);
        unsafe {
            self.lock.unlock();
        }
        mem
    }

    pub fn workers(&self) -> &Pool {
        &self.scoped_pool
    }

    pub fn marking_context(&self) -> &MarkingContext {
        &self.marking_context
    }

    pub(crate) fn marking_context_mut(&mut self) -> &mut MarkingContext {
        &mut self.marking_context
    }

    pub(crate) fn allocate_memory(&mut self, req: &mut AllocRequest) -> *mut u8 {
        let mut in_new_region = false;
        let mut result;

        result = self.allocate_memory_under_lock(req, &mut in_new_region);

        // Allocation failed, block until control thread reacted, then retry allocation.
        //
        // It might happen that one of the threads requesting allocation would unblock
        // way later after GC happened, only to fail the second allocation, because
        // other threads have already depleted the free storage. In this case, a better
        // strategy is to try again, as long as GC makes progress.
        //
        // Then, we need to make sure the allocation was retried after at least one
        // Full GC, which means we want to try more than 3 times.
        let mut tries = 0;

        /*while result.is_null() && self.progress_last_gc.is_set() {
            tries += 1;
            self.controller_thread().handle_alloc_failure_gc(req);
            result = self.allocate_memory_under_lock(req, &mut in_new_region);
        }*/

        while result.is_null() && tries <= 3 {
            tries += 1;
            self.controller_thread().handle_alloc_failure_gc(req);

            result = self.allocate_memory_under_lock(req, &mut in_new_region);
        }

        if in_new_region {
            self.controller_thread().notify_heap_changed();
        }

        if !result.is_null() {
            let requested = req.size();
            let actual = req.actual_size();

            assert!(
                req.for_lab() || requested == actual,
                "Only LAB allocations are elastic, requested {}, actual = {}",
                formatted_size(requested),
                formatted_size(actual)
            );

            self.notify_mutator_alloc(actual, false);
        }
        result
    }

    pub(crate) fn rebuild_free_set(&mut self, concurrent: bool) {
        let _ = concurrent;
        self.free_set.rebuild();
    }

    pub unsafe fn heap_region_iterate(&self, blk: &dyn HeapRegionClosure) {
        for i in 0..self.num_regions() {
            let current = self.get_region(i);
            blk.heap_region_do(current);
        }
    }

    pub unsafe fn parallel_heap_region_iterate(&self, blk: &dyn HeapRegionClosure) {
        if self.num_regions() > self.options().parallel_region_stride
            && self.scoped_pool.workers() != 0
        {
            let task = ParallelHeapRegionTask {
                blk,
                index: AtomicUsize::new(0),
            };
            self.scoped_pool.scoped(move |scope| {
                task.work(scope);
            });
        } else {
            self.heap_region_iterate(blk);
        }
    }

    pub(crate) unsafe fn parallel_heap_region_iterate_with_cancellation(
        &self,
        blk: &dyn HeapRegionClosure,
    ) {
        if self.num_regions() > self.options().parallel_region_stride
            && self.scoped_pool.workers() != 0
        {
            let task = ParallelHeapRegionTaskWithCancellationCheck {
                blk,
                index: AtomicUsize::new(0),
            };
            self.scoped_pool.scoped(move |scope| {
                task.work(scope);
            });
        } else {
            self.heap_region_iterate(blk);
        }
    }

    pub(crate) unsafe fn trash_humongous_region_at(&self, start: *mut HeapRegion) {
        let humongous_obj = (*start).bottom() as *mut HeapObjectHeader;
        let sz = (*humongous_obj).heap_size();
        let required_regions = self.options().required_regions(sz);

        let mut index = (*start).index() + required_regions - 1;

        for _ in 0..required_regions {
            let region = self.get_region(index);
            index -= 1;

            (*region).make_trash();
        }
    }

    /// Mark address as live object.
    ///
    /// # Safety
    ///
    /// Caller must ensure that `addr` points to valid object.
    pub unsafe fn mark_live(&self, addr: *mut u8) {
        unsafe {
            let index = self.region_index(addr);
            let region = self.get_region(index);

            if (*region).is_humongous_start() {
                // no-op
                return;
            }

            if (*region).is_humongous_cont() {
                debug_assert!(
                    false,
                    "Humongous object should be marked as live by its start region"
                );
            }

            let bitmap = &(*region).object_start_bitmap;
            bitmap.set_bit(addr as usize);
        }
    }

    pub fn is_live(&self, addr: *mut u8) -> bool {
        if self.mem.contains(addr as _) {
            return false;
        }
        let index = self.region_index(addr);
        let region = self.get_region(index);

        unsafe {
            if (*region).is_humongous_start() {
                return true;
            }

            if (*region).is_humongous_cont() {
                return true;
            }

            let bitmap = &(*region).object_start_bitmap;
            bitmap.check_bit(addr as usize)
        }
    }

    pub(crate) fn prepare_gc(&mut self) {
        self.reset_mark_bitmap();
    }

    pub(crate) fn reset_mark_bitmap(&mut self) {
        //self.marking_context.flip_mark_bit();
        self.marking_context.clear_bitmap_full();
    }

    pub unsafe fn get_humongous_start(&self, mut r: *mut HeapRegion) -> *mut HeapRegion {
        let mut i = (*r).index();

        while !(*r).is_humongous_start() {
            i -= 1;
            r = self.get_region(i);
        }

        r
    }

    pub fn is_in(&self, addr: *mut u8) -> bool {
        self.mem.contains(addr as _)
    }

    pub fn object_start(&self, addr: *mut u8) -> *mut HeapObjectHeader {
        if !self.mem.contains(addr as _) {
            return null_mut();
        }
        let index = self.region_index(addr);
        let region = self.get_region(index);

        unsafe {
            if (*region).is_humongous_start() {
                return (*region).bottom() as *mut HeapObjectHeader;
            }

            if (*region).is_humongous_cont() {
                let start = self.get_humongous_start(region);
                return (*start).bottom() as *mut HeapObjectHeader;
            }

            let bitmap = &(*region).object_start_bitmap;
            let start = bitmap.find_object_start(addr as _);
            start as *mut HeapObjectHeader
        }
    }

    pub fn tlab_capacity(&self) -> usize {
        self.free_set().capacity()
    }

    pub unsafe fn stop(&mut self) {
        self.cancel_gc();
        self.controller_thread.as_mut().unwrap().stop();
        let _ = unsafe { Box::from_raw(self.controller_thread.take().unwrap()) };
        let _ = Box::from_raw(self as *mut Self);
        log::debug!("Heap stopped");
        INIT.store(false, Ordering::Release);
    }

    pub fn print_on<W: std::fmt::Write>(&self, st: &mut W) -> std::fmt::Result {
        write!(
            st,
            "Heap {} max, {} commited, {} x {} regions",
            formatted_size(self.max_capacity()),
            formatted_size(self.get_commited()),
            self.num_regions(),
            formatted_size(self.options().region_size_bytes)
        )?;

        Ok(())
    }

    pub fn try_cancel_gc(&self) -> bool {
        loop {
            let prev = self.cancelled_gc.cmpxchg(GC_CANCELLABLE, GC_CANCELLED);
            if prev == GC_CANCELLABLE {
                return true;
            } else if prev == GC_CANCELLED {
                return false;
            }

            let thread = Thread::current();
            if thread.is_registered() {
                // We need to provide a safepoint here, otherwise we might
                // spin forever if a SP is pending.
                safepoint_scope(|| {
                    std::hint::spin_loop();
                });
            }
        }
    }

    pub fn cancel_gc(&self) {
        self.try_cancel_gc();
    }

    pub(crate) fn cancelled_gc_address(&self) -> *const AtomicU8 {
        self.cancelled_gc.addr_of()
    }

    pub fn cancelled_gc(&self) -> bool {
        self.cancelled_gc.get() == GC_CANCELLED
    }

    pub(crate) fn check_cancelled_gc_and_yield(&self, sts_active: bool) -> bool {
        if !sts_active {
            return self.cancelled_gc();
        }

        let prev = self.cancelled_gc.cmpxchg(GC_CANCELLABLE, GC_NOT_CANCELLED);

        if prev == GC_CANCELLABLE || prev == GC_NOT_CANCELLED {
            if SuspendibleThreadSet::should_yield() {
                SuspendibleThreadSet::yield_();
            }

            if prev == GC_CANCELLABLE {
                self.cancelled_gc.set(GC_CANCELLABLE);
            }
            return false;
        }
        true
    }

    pub fn clear_cancelled_gc(&self) {
        self.cancelled_gc.set(GC_CANCELLABLE);
    }

    pub(crate) fn safepoint_synchronize_begin(&self) {
        SuspendibleThreadSet::synchronize();
    }

    pub(crate) fn safepoint_synchronize_end(&self) {
        SuspendibleThreadSet::desynchronize();
    }

    pub fn add_core_root_set(&mut self) {
        self.add_root(SimpleRoot::new(
            "Conservative roots",
            "CS",
            |processor| unsafe {
                assert!(
                    SafepointSynchronize::is_at_safepoint(),
                    "must be at safepoint for scanning thread stacks and registers"
                );
                let threads = threads();

                for thread in threads.threads.unsafe_get() {
                    let thread = &**thread;
                    processor.add_task(
                        move |processor| {
                            let mut stack_start = thread.stack_start();
                            let mut stack_end = thread.last_sp();

                            if stack_start > stack_end {
                                // stack grows in different direction
                                std::mem::swap(&mut stack_start, &mut stack_end);
                            }

                            processor.visitor().visit_conservative(
                                stack_start.cast(),
                                (stack_end as usize - stack_start as usize) / size_of::<usize>(),
                            );
                            let (regs, size) = (*thread).get_registers();
                            if !regs.is_null() && size != 0 {
                                processor.visitor().visit_conservative(regs.cast(), size);
                            }
                        },
                        false,
                    );
                }
            },
        ));
    }
}

struct HeapRef(&'static mut Heap);

unsafe impl Send for HeapRef {}
unsafe impl Sync for HeapRef {}

static INIT: AtomicBool = AtomicBool::new(false);

static mut HEAP: MaybeUninit<HeapRef> = MaybeUninit::uninit();

pub fn heap() -> &'static mut Heap {
    #[cold]
    fn heap_uninit() {
        panic!("heap() called before heap initialized")
    }
    if !INIT.load(Ordering::Acquire) {
        heap_uninit();
    }
    unsafe { HEAP.assume_init_mut().0 }
}

pub trait HeapRegionClosure: Send + Sync {
    fn heap_region_do(&self, r: *mut HeapRegion);
    fn after_processing(&self, count: usize) {
        let _ = count;
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let _ = unsafe { Box::from_raw(self.marking_context) };
    }
}

pub struct ParallelHeapRegionTask<'a> {
    blk: &'a dyn HeapRegionClosure,
    index: AtomicUsize,
}

unsafe impl<'a> Send for ParallelHeapRegionTask<'a> {}
unsafe impl<'a> Sync for ParallelHeapRegionTask<'a> {}

impl<'a> ParallelHeapRegionTask<'a> {
    pub fn work<'b>(self, scope: &'b scoped_thread_pool::Scope<'a>) {
        let heap = heap();
        let stride = heap.options().parallel_region_stride;

        let max = heap.num_regions();
        while self.index.load(Ordering::Relaxed) < max {
            let cur = self.index.fetch_add(stride, Ordering::Relaxed);
            let start = cur;
            let end = max.min(cur + stride);

            if start >= max {
                break;
            }
            scope.execute(move || {
                let heap = super::heap::heap();
                for i in cur..end {
                    let region = heap.get_region(i);
                    self.blk.heap_region_do(region);
                }

                self.blk.after_processing(end - cur);
            });
        }
    }
}

pub struct ParallelHeapRegionTaskWithCancellationCheck<'a> {
    blk: &'a dyn HeapRegionClosure,
    index: AtomicUsize,
}

impl<'a> ParallelHeapRegionTaskWithCancellationCheck<'a> {
    pub fn work<'b>(self, scope: &'b scoped_thread_pool::Scope<'a>) {
        let heap = heap();
        let stride = heap.options().parallel_region_stride;

        let max = heap.num_regions();
        while self.index.load(Ordering::Relaxed) < max {
            let cur = self.index.fetch_add(stride, Ordering::Relaxed);
            let start = cur;
            let end = max.min(cur + stride);

            if start >= max || heap.cancelled_gc() {
                break;
            }
            scope.execute(move || {
                let heap = super::heap::heap();
                if heap.cancelled_gc() {
                    return;
                }
                for i in cur..end {
                    if heap.cancelled_gc() {
                        return;
                    }
                    let region = heap.get_region(i);
                    self.blk.heap_region_do(region);
                }

                self.blk.after_processing(end - cur);
            });
        }
    }
}

pub struct ResetBitmapClosure {
    heap: &'static Heap,
}

unsafe impl Send for ResetBitmapClosure {}
unsafe impl Sync for ResetBitmapClosure {}

impl HeapRegionClosure for ResetBitmapClosure {
    fn heap_region_do(&self, _r: *mut HeapRegion) {
        //self.heap.marking_context().flip_mark_bit();
    }
}
