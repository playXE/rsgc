use std::{mem::{size_of, MaybeUninit}, ptr::null_mut, sync::atomic::{AtomicUsize, Ordering}, time::Instant};

use crate::formatted_size;

use super::{
    bitmap::HeapBitmap,
    free_set::RegionFreeSet,
    region::{HeapRegion, HeapOptions},
    safepoint,
    virtual_memory::{self, VirtualMemory},
    AllocRequest, controller::ControlThread,
};
use parking_lot::{lock_api::RawMutex, RawMutex as Lock};

pub struct Heap {
    pub(crate) lock: Lock,
    mem: Box<VirtualMemory>,
    regions_storage: Box<VirtualMemory>,
    regions: Vec<*mut HeapRegion>,
    opts: HeapOptions,
    free_set: RegionFreeSet,
    used: AtomicUsize,
    bytes_allocated_since_gc_start: AtomicUsize,
    last_cycle_end: Instant,
    controller_thread: Option<&'static mut ControlThread>,
}

impl Heap {
    pub fn free_set(&self) -> &RegionFreeSet {
        &self.free_set
    }

    pub fn free_set_mut(&mut self) -> &mut RegionFreeSet {
        &mut self.free_set
    }

    pub fn options(&self) -> &HeapOptions {
        &self.opts
    }

    pub fn new(
        max_heap_size: usize,
        min_region_size: Option<usize>,
        max_region_size: Option<usize>,
        region_size: Option<usize>,
        target_num_regions: Option<usize>,
        humongous_threshold: Option<usize>,
        min_tlab_size: Option<usize>,
        elastic_tlab: bool,
        min_free_threshold: usize,
        allocation_threshold: usize,
    ) -> &'static mut Self {
        safepoint::init();
        let mut opts = HeapRegion::setup_sizes(
            max_heap_size,
            min_region_size,
            target_num_regions,
            max_region_size,
            humongous_threshold,
            region_size,
            elastic_tlab,
            min_tlab_size.unwrap_or(2 * 1024),
        );

        opts.min_free_threshold = min_free_threshold;
        opts.allocation_threshold = allocation_threshold;
        let mem = VirtualMemory::allocate_aligned(
            opts.max_heap_size,
            opts.region_size_bytes,
            false,
            false,
            "heap",
        )
        .unwrap();

        let regions_mem = VirtualMemory::allocate_aligned(
            opts.region_count * size_of::<HeapRegion>(),
            virtual_memory::page_size(),
            false,
            false,
            "heap regions",
        )
        .unwrap();
        let free_set = RegionFreeSet::new(&opts);

        let this = Box::leak(Box::new(Self {
            regions: vec![null_mut(); opts.region_count],
            regions_storage: regions_mem,
            mem,
            opts,
            lock: Lock::INIT,
            free_set,
            used: AtomicUsize::new(0),
            bytes_allocated_since_gc_start: AtomicUsize::new(0),
            last_cycle_end: Instant::now(),
            controller_thread: None,
        }));

        let ptr = this as *mut Heap;
        for i in 0..opts.region_count {
            let start = this.mem.start() + this.opts.region_size_bytes * i;
            let loc = this.regions_storage.start() + i * size_of::<HeapRegion>();

            unsafe {
                let region = HeapRegion::new(loc, i, start, &this.opts);
                this.regions[i] = region;
            }
        }

        this.free_set.set_heap(ptr);
        this.free_set.rebuild();

        unsafe {
            HEAP = MaybeUninit::new(HeapRef(&mut *(this as *mut Self)));

            heap().controller_thread = Some(ControlThread::new());
        }
        this
    }

    pub fn controller_thread<'a>(&'a self) -> &'a ControlThread {
        self.controller_thread.as_ref().unwrap()
    }

    pub fn increase_used(&self, bytes: usize) {
        self.used.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn decrease_used(&self, bytes: usize) {
        self.used.fetch_sub(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_used(&self, bytes: usize) {
        self.used.store(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn increase_allocated(&self, bytes: usize) {
        self.bytes_allocated_since_gc_start
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn decrease_allocated(&self, bytes: usize) {
        self.bytes_allocated_since_gc_start
            .fetch_sub(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_allocated(&self, bytes: usize) {
        self.bytes_allocated_since_gc_start
            .store(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn notify_mutator_alloc(&self, bytes: usize, waste: bool) {
        if !waste {
            self.increase_used(bytes);
        }

        self.increase_allocated(bytes);
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

    pub fn get_region(&self, index: usize) -> *mut HeapRegion {
        self.regions[index]
    }
    /* 
    pub fn allocate_memory(&mut self, req: &mut AllocRequest, in_new_region: bool) -> *mut u8 {
        self.allocate_memory_under_lock(req, in_new_region)
    }

    pub unsafe fn free_single(&mut self, addr: *mut u8, size: usize) {
        self.lock.lock();
        self.free_set.free_single(addr, size);
        unsafe {
            self.lock.unlock();
        }
    }

    fn allocate_memory_under_lock(
        &mut self,
        req: &mut AllocRequest,
        in_new_region: bool,
    ) -> *mut u8 {
        self.lock.lock();
        let result = self.free_set.allocate(req, in_new_region);
        unsafe {
            self.lock.unlock();
        }

        result
    }

    pub fn allocate_tlab(&mut self) -> (*mut u8, usize) {
        let mut req = AllocRequest::new(self.options().min_tlab_size, self.options().min_tlab_size);

        // guaranteed to not allocate a humongous object here
        let mem = self.allocate_memory(
            &mut req, true, /* always allocate new region for TLAB if needed */
        );

        if mem.is_null() {
            return (null_mut(), 0);
        }
        let size;
        if req.actual_size() > self.options().max_tlab_size {
            // if returned block of memory is too large simply return unused memory
            // back to free-list
            unsafe {
                let remainder = mem.add(self.options().max_tlab_size);
                let remainder_size = mem.add(req.actual_size()) as usize - remainder as usize;
                self.free_single(remainder, remainder_size);
            }
            size = self.options().max_tlab_size;
        } else if req.actual_size() < self.options().min_tlab_size {
            // no enough memory left for TLAB creation, return null.
            // Triggers GC
            unsafe {
                self.free_single(mem, req.actual_size());
            }

            return (null_mut(), 0);
        } else {
            // size of block lays somewhere in between of [min_tlab_size,max_tlab_size],
            // we're happy with it and return memory to thread that requested this TLAB
            size = req.actual_size()
        }

        (mem, size)
    }*/

    pub fn max_capacity(&self) -> usize {
        self.regions.len() * self.options().region_size_bytes
    }

    pub fn should_start_gc(&self) -> bool {
        let max_capacity = self.max_capacity();
        let available = self.free_set().available();

        let threshold_available = max_capacity / 100 * self.options().min_free_threshold;
        let threshold_bytes_allocated = max_capacity / 100 * self.options().allocation_threshold;
        if available < threshold_available {
            log::info!(target: "gc",
                "Trigger: Free ({}) is below minimum threshold ({})",
                formatted_size(available),
                formatted_size(threshold_available)
            );
            return true; 
        }
        
        let bytes_allocated = self.bytes_allocated_since_gc_start.load(Ordering::Relaxed);

        if bytes_allocated > threshold_bytes_allocated {
            log::info!(target: "gc", 
                "Trigger: Allocated since last cycle ({}) is larger than allocation threshold ({})",
                formatted_size(bytes_allocated),
                formatted_size(threshold_bytes_allocated)
            );
            return true;
        }

        if self.options().guaranteed_gc_interval > 0 {
            let last_time_ms = self.last_cycle_end.elapsed().as_millis();

            if last_time_ms > self.options().guaranteed_gc_interval as _ {
                log::info!(target: "gc",
                    "Trigger: Time since last GC ({} ms) is larger than guaranteed interval ({} ms)",
                    last_time_ms,
                    self.options().guaranteed_gc_interval
                );
                return true;    
            }
        }

        false
    }
}

struct HeapRef(&'static mut Heap);

unsafe impl Send for HeapRef {}
unsafe impl Sync for HeapRef {}

static mut HEAP: MaybeUninit<HeapRef> = MaybeUninit::uninit();

pub fn heap() -> &'static mut Heap {
    unsafe {
        HEAP.assume_init_mut().0
    }
}