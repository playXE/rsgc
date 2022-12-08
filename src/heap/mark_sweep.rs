use std::mem::size_of;
use std::sync::atomic::AtomicUsize;

use super::heap::{heap, Heap};
use super::safepoint;
use crate::heap::controller::GCMode;
use crate::heap::sweeper::SweepGarbageClosure;
use crate::heap::thread::wait_for_the_world;
use crate::heap::thread::ThreadInfo;
use crate::object::HeapObjectHeader;
use crate::traits::Visitor;
use parking_lot::lock_api::RawMutex;
use parking_lot::MutexGuard;
pub struct MarkSweep {
    heap: &'static mut Heap,
    mark_stack: Vec<*mut HeapObjectHeader>,
}

impl MarkSweep {
    pub fn new() -> Self {
        Self {
            heap: heap(),
            mark_stack: Vec::with_capacity(128)
        }
    }

    pub unsafe fn do_collect(&mut self, mode: GCMode) {
        let start = std::time::Instant::now();
        while !safepoint::enter() {
            // Users can use our safepoint infrastructure as well and safepoint::enter() can return false
            // in cases like cleaning JIT caches on the user side.
            std::thread::yield_now();
        }
        
        let mut threads = wait_for_the_world();
        log::debug!(target: "gc-safepoint", "stopped the world in {} ms", start.elapsed().as_millis());
        // Phase 0: retire TLABs
        for thread in threads.iter().copied() {
            (*thread).tlab.retire();
        }
        {
            // Phase 1: Mark
            self.do_mark(&mut threads);
        }

        {
            // Phase 2: Process weak references
            self.heap.process_weak_refs();
        }

        // Phase 3: Sweep
        let closure = SweepGarbageClosure {
            concurrent: mode == GCMode::ConcurrentSweep,
            heap: heap(),
            live: AtomicUsize::new(0),
        };
        match mode {
            GCMode::FullSTW => {
                
                if self.heap.options().parallel_sweep {
                    self.heap.parallel_heap_region_iterate(&closure);
                } else {
                    self.heap.heap_region_iterate(&closure);
                }
                
                
            
                self.heap.free_set_mut().recycle_trash();
                self.heap.lock.lock();
                self.heap.free_set_mut().rebuild();
                self.heap.lock.unlock();
            }

            GCMode::ConcurrentSweep => {
                if self.heap.options().parallel_sweep {
                    self.heap.parallel_heap_region_iterate(&closure);
                } else {
                    self.heap.heap_region_iterate(&closure);
                }
                
                self.heap.lock.lock();
                self.heap.free_set_mut().recycle_trash();
                self.heap.free_set_mut().rebuild();
                self.heap.lock.unlock();
            }
            _ => unreachable!(),
        }

        log::debug!(target: "gc", "Mark-and-Sweep done in {} ms, {} live regions after sweep", start.elapsed().as_millis(), closure.live.load(std::sync::atomic::Ordering::Relaxed));
        safepoint::end();
    }

    unsafe fn do_mark(&mut self, threads: &mut MutexGuard<'_, Vec<*mut ThreadInfo>>) {
        for thread in threads.iter().copied() {
            self.mark_thread(thread);
        }

        heap().walk_global_roots(self);

         
        if !self.heap.options().parallel_mark || self.heap.workers().workers() == 0 {
            while let Some(object) = self.mark_stack.pop() {
                (*object).visit(self);
            }
        } else {
            self.start_parallel_marking();
        }
    }

    unsafe fn start_parallel_marking(&mut self) {
        let nworkers = self.heap.workers().workers();
        let mut workers = Vec::with_capacity(nworkers);
        let mut stealers = Vec::with_capacity(nworkers);

        let injector = Injector::new();

        for _ in 0..nworkers {
            let w = Worker::new_lifo();
            let s = w.stealer();
            workers.push(w);
            stealers.push(s);
        }

        for obj in self.mark_stack.drain(..) {
            injector.push(obj as usize);
        }

        let terminator = Terminator::new(nworkers);

        self.heap.workers().scoped(|scope| {
            for (task_id, worker) in workers.into_iter().enumerate() {
                let injector = &injector;
                let stealers = &stealers;
                let terminator = &terminator;

                scope.execute(move || {
                    let mut task = MarkingTask {
                        task_id,
                        local: Segment::new(),
                        worker,
                        injector,
                        stealers,
                        terminator,
                        marked: 0,
                        heap: heap()
                    };
    
                    task.run();
                });
            }
        });


    }

    unsafe fn mark_thread(&mut self, thread: *mut ThreadInfo) {
        let mut begin = (*thread).stack_start();
        let mut end = (*thread).last_sp();
        if end.is_null() {
            return;
        }
        if begin > end {
            std::mem::swap(&mut begin, &mut end);
        }
        
        while begin < end {
            self.try_mark_conservatively(begin.cast::<*mut u8>().read());
            begin = begin.add(size_of::<usize>());
        }
    }

    fn try_mark(&mut self, object: *const u8) {
        unsafe {
            let header = object.sub(size_of::<HeapObjectHeader>()) as *mut HeapObjectHeader;

            if (*header).is_marked() {
                return;
            }

            (*header).set_marked();

            self.mark_stack.push(header);
        }
    }

    fn try_mark_conservatively(&mut self, pointer: *mut u8) {
        unsafe {
            if !self.heap.is_in(pointer) {
                return;
            }
            
            let object = self.heap.object_start(pointer);
            if object.is_null() {
                return;
            }

            self.try_mark(object.add(1) as *const u8);
        }
    }
}

impl Visitor for MarkSweep {
    fn visit(&mut self, object: *const u8) {
        self.try_mark(object);
    }

    fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize) {
        for i in 0..len {
            unsafe {
                let ptr = *ptrs.add(i);
                self.try_mark_conservatively(ptr as *mut u8);
            }
        }
    }
}

use std::sync::atomic::{Ordering};
use std::thread;
use std::time::Duration;
use rand::distributions::{Distribution, Uniform};
use rand::thread_rng;
use crossbeam_deque::{Injector, Steal, Stealer, Worker};

pub struct Terminator {
    const_nworkers: usize,
    nworkers: AtomicUsize,
}

impl Terminator {
    pub fn new(number_workers: usize) -> Terminator {
        Terminator {
            const_nworkers: number_workers,
            nworkers: AtomicUsize::new(number_workers),
        }
    }

    pub fn try_terminate(&self) -> bool {
        if self.const_nworkers == 1 {
            return true;
        }

        if self.decrease_workers() {
            // reached 0, no need to wait
            return true;
        }

        thread::sleep(Duration::from_micros(1));
        self.zero_or_increase_workers()
    }

    fn decrease_workers(&self) -> bool {
        self.nworkers.fetch_sub(1, Ordering::Relaxed) == 1
    }

    fn zero_or_increase_workers(&self) -> bool {
        let mut nworkers = self.nworkers.load(Ordering::Relaxed);

        loop {
            if nworkers == 0 {
                return true;
            }

            let result = self.nworkers.compare_exchange(
                nworkers,
                nworkers + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );

            match result {
                Ok(_) => {
                    // Value was successfully increased again, workers didn't terminate in
                    // time. There is still work left.
                    return false;
                }

                Err(prev_nworkers) => {
                    nworkers = prev_nworkers;
                }
            }
        }
    }
}

type Address = usize;

struct MarkingTask<'a> {
    task_id: usize,
    local: Segment,
    worker: Worker<Address>,
    injector: &'a Injector<Address>,
    stealers: &'a [Stealer<Address>],
    terminator: &'a Terminator,
    marked: usize,
    heap: &'static Heap,
}


impl<'a> MarkingTask<'a> {
    fn pop(&mut self) -> Option<Address> {
        self.pop_local()
            .or_else(|| self.pop_worker())
            .or_else(|| self.pop_global())
            .or_else(|| self.steal())
    }

    fn pop_local(&mut self) -> Option<Address> {
        if self.local.is_empty() {
            return None;
        }

        let obj = self.local.pop().expect("should be non-empty");
        Some(obj)
    }

    fn pop_worker(&mut self) -> Option<Address> {
        self.worker.pop()
    }

    fn pop_global(&mut self) -> Option<Address> {
        loop {
            let result = self.injector.steal_batch_and_pop(&mut self.worker);

            match result {
                Steal::Empty => break,
                Steal::Success(value) => return Some(value),
                Steal::Retry => continue,
            }
        }

        None
    }

    fn steal(&self) -> Option<Address> {
        if self.stealers.len() == 1 {
            return None;
        }

        let mut rng = thread_rng();
        let range = Uniform::new(0, self.stealers.len());

        for _ in 0..2 * self.stealers.len() {
            let mut stealer_id = self.task_id;

            while stealer_id == self.task_id {
                stealer_id = range.sample(&mut rng);
            }

            let stealer = &self.stealers[stealer_id];

            loop {
                match stealer.steal_batch_and_pop(&self.worker) {
                    Steal::Empty => break,
                    Steal::Success(address) => return Some(address),
                    Steal::Retry => continue,
                }
            }
        }

        None
    }

    fn defensive_push(&mut self) {
        self.marked += 1;

        if self.marked > 256 {
            if self.local.len() > 4 {
                let target_len = self.local.len() / 2;

                while self.local.len() > target_len {
                    let val = self.local.pop().unwrap();
                    self.injector.push(val);
                }
            }

            self.marked = 0;
        }
    }

    fn run(&mut self) {
        loop {
            let object_addr = if let Some(addr) = self.pop() {
                addr 
            } else if self.terminator.try_terminate() {
                break;
            } else {
                continue;
            };

            let object = object_addr as *mut HeapObjectHeader;
            unsafe {
                (*object).visit(self);
            }
        } 
    }

    unsafe fn try_mark(&mut self, obj: *mut u8) 
    {
        let obj = obj.cast::<HeapObjectHeader>().sub(1);
        if (*obj).try_mark() {
            if self.local.has_capacity() {
                self.local.push(obj as _);
                self.defensive_push();
            } else {
                self.worker.push(obj as _);
            }
        }
    }


    unsafe fn try_mark_conservatively(&mut self, obj: *const u8) {
        if !self.heap.is_in(obj as _) {
            return;
        }
        
        let object = self.heap.object_start(obj as _);
        if object.is_null() {
            return;
        }

        self.try_mark(object.add(1) as *mut u8);
    }
}

impl<'a> Visitor for MarkingTask<'a> {
    fn visit(&mut self, object: *const u8) {
        unsafe {
            self.try_mark(object as _);
        }
    }

    fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize) {
        unsafe {
            for i in 0..len {
                self.try_mark_conservatively(ptrs.add(i).read());
            }
        }
    }
}

const SEGMENT_SIZE: usize = 64;

struct Segment {
    data: Vec<Address>,
}

impl Segment {
    fn new() -> Segment {
        Segment {
            data: Vec::with_capacity(SEGMENT_SIZE),
        }
    }

    fn empty() -> Segment {
        Segment { data: Vec::new() }
    }

    fn with(addr: usize) -> Segment {
        let mut segment = Segment::new();
        segment.data.push(addr);

        segment
    }

    fn has_capacity(&self) -> bool {
        self.data.len() < SEGMENT_SIZE
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn push(&mut self, addr: Address) {
        debug_assert!(self.has_capacity());
        self.data.push(addr);
    }

    fn pop(&mut self) -> Option<Address> {
        self.data.pop()
    }

    fn len(&mut self) -> usize {
        self.data.len()
    }
}