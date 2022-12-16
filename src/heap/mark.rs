use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use rand::distributions::{Distribution, Uniform};
use rand::thread_rng;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use crate::object::HeapObjectHeader;
use crate::sync::suspendible_thread_set::SuspendibleThreadSetLeaver;
use crate::traits::Visitor;

use super::heap::{heap, Heap};
use super::marking_context::MarkingContext;
use super::root_processor::RootsCollector;
use super::thread::ThreadInfo;

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

pub struct MarkingTask<'a> {
    task_id: usize,
    local: Segment,
    /*worker: &'a Worker<Address>,
    injector: &'a Injector<Address>,
    stealers: &'a [Stealer<Address>],*/
    terminator: &'a Terminator,
    marked: usize,
    pub(crate) heap: &'static Heap,
    pub(crate) mark_ctx: &'static MarkingContext,
}

impl<'a> MarkingTask<'a> {
    pub fn new(
        task_id: usize,
        terminator: &'a Terminator,
        heap: &'static Heap,
        mark_ctx: &'static MarkingContext,
    ) -> MarkingTask<'a> {
        MarkingTask {
            task_id,
            local: Segment::new(),
            terminator,
            marked: 0,
            heap,
            mark_ctx,
        }
    }

    pub fn pop(&mut self) -> Option<Address> {
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
        self.mark_ctx.mark_queues().worker(self.task_id).pop()
    }

    fn worker(&self) -> &Worker<usize> {
        self.mark_ctx.mark_queues().worker(self.task_id)
    }

    fn stealers(&self) -> &[Stealer<usize>] {
        self.mark_ctx.mark_queues().stealers()
    }

    fn injector(&self) -> &Injector<usize> {
        self.mark_ctx.mark_queues().injector()
    }

    fn pop_global(&mut self) -> Option<Address> {
        loop {
            let result = self
                .mark_ctx
                .mark_queues()
                .injector()
                .steal_batch_and_pop(self.worker());

            match result {
                Steal::Empty => break,
                Steal::Success(value) => return Some(value),
                Steal::Retry => continue,
            }
        }

        None
    }

    fn steal(&self) -> Option<Address> {
        if self.stealers().len() == 1 {
            return None;
        }

        let mut rng = thread_rng();
        let range = Uniform::new(0, self.stealers().len());

        for _ in 0..2 * self.stealers().len() {
            let mut stealer_id = self.task_id;

            while stealer_id == self.task_id {
                stealer_id = range.sample(&mut rng);
            }

            let stealer = &self.stealers()[stealer_id];

            loop {
                match stealer.steal_batch_and_pop(self.worker()) {
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
                    self.injector().push(val);
                }
            }

            self.marked = 0;
        }
    }

    pub fn run<const CANCELLABLE: bool>(&mut self) {
        loop {
            let object_addr = if let Some(addr) = self.pop() {
                addr
            } else if CANCELLABLE && self.heap.check_cancelled_gc_and_yield(true) {
                break;
            } else {
                let stsl = SuspendibleThreadSetLeaver::new(CANCELLABLE);
                if self.terminator.try_terminate() {
                    break;
                }
                drop(stsl);
                continue;
            };

            let object = object_addr as *mut HeapObjectHeader;
            unsafe {
                (*object).visit(self);
            }
        }
    }

    pub fn push(&mut self, obj: *mut HeapObjectHeader) {
        if self.local.has_capacity() {
            self.local.push(obj as _);
            self.defensive_push();
        } else {
            self.worker().push(obj as _);
        }
    }

    pub unsafe fn try_mark(&mut self, obj: *mut u8) {
        let obj = obj.cast::<HeapObjectHeader>().sub(1);
        if self.mark_ctx.mark(obj) {
            self.push(obj);
        }
    }

    pub unsafe fn try_mark_conservatively(&mut self, obj: *const u8) {
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

pub struct MarkQueueSet {
    workers: Vec<Worker<usize>>,
    stealers: Vec<Stealer<usize>>,
    injector: Injector<usize>,
}

impl MarkQueueSet {
    pub fn new(nworkers: usize) -> MarkQueueSet {
        let mut workers = Vec::with_capacity(nworkers);
        let mut stealers = Vec::with_capacity(nworkers);

        let injector = Injector::new();

        for _ in 0..nworkers {
            let w = Worker::new_lifo();
            let s = w.stealer();
            workers.push(w);
            stealers.push(s);
        }

        MarkQueueSet {
            workers,
            stealers,
            injector,
        }
    }

    pub fn worker(&self, id: usize) -> &Worker<usize> {
        &self.workers[id]
    }

    pub fn stealer(&self, id: usize) -> &Stealer<usize> {
        &self.stealers[id]
    }

    pub fn stealers(&self) -> &[Stealer<usize>] {
        &self.stealers
    }

    pub fn nworkers(&self) -> usize {
        self.workers.len()
    }

    pub fn injector(&self) -> &Injector<usize> {
        &self.injector
    }
}

pub struct STWMark {}

impl STWMark {
    pub fn new() -> Self {
        Self {}
    }

    pub fn mark(&self, threads: &[*mut ThreadInfo]) {
        let heap = heap();
        let mut mark_stack = {
            let mut root_collector = RootsCollector::new(heap);
            root_collector.collect(threads);
            root_collector.get_mark_stack()
        };

        let injector = heap.marking_context().mark_queues().injector();
        while let Some(obj) = mark_stack.pop() {
            injector.push(obj as usize);
        }

        let mc = heap.marking_context();

        let terminator = Terminator::new(mc.mark_queues().nworkers());

        // blocking call, waits for marking task to complete.
        heap.workers().scoped(|scope| {
            for task_id in 0..mc.mark_queues().nworkers() {
                let terminator = &terminator;

                scope.execute(move || {
                    let mut task = MarkingTask::new(
                        task_id,
                        &terminator,
                        super::heap::heap(),
                        super::heap::heap().marking_context(),
                    );

                    task.run::<false>();
                });
            }
        });
    }
}
