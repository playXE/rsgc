//! Marking phase of the GC.
//! 
//! Implements simple marking algorithm that uses a work stealing queue to distribute marking work between worker threads.
//! Marking tasks can be terminated if GC is cancelled which allows us to start STW GC as soon as possible.

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use rand::distributions::{Distribution, Uniform};
use rand::thread_rng;
use std::mem::size_of;
use std::ptr::null;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::sync::suspendible_thread_set::SuspendibleThreadSetLeaver;
use crate::system::object::HeapObjectHeader;
use crate::system::traits::Visitor;

use super::heap::{heap, Heap};
use super::marking_context::MarkingContext;
use super::thread::Thread;

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
    visitor: &'a mut SlotVisitor,
    terminator: &'a Terminator,
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
            visitor: unsafe { &mut *(mark_ctx.mark_queues().visitor(task_id)) },

            terminator,
            heap,
            mark_ctx,
        }
    }

    pub fn run<const CANCELLABLE: bool>(&mut self) {
        let stride = self.heap.options().mark_loop_stride;

        loop {
            if CANCELLABLE && self.heap.check_cancelled_gc_and_yield(true) {
                break;
            }
            let mut work = 0;
            for _ in 0..stride {
                if let Some(task) = self.visitor.pop() {
                    unsafe {
                        self.do_task(task);
                    }
                    work += 1;
                } else {
                    break;
                }
            }

            if work == 0 {
                let stsl = SuspendibleThreadSetLeaver::new(CANCELLABLE);
                if self.terminator.try_terminate() {
                    break;
                }
                drop(stsl);
                continue;
            }
        }
    }

    unsafe fn do_task(&mut self, task: MarkTask) {
        let obj = task.obj();
        if task.is_not_chunked() {
            if !(*obj).vtable().varsize.is_varsize {
                (*obj).visit(self.visitor);
            } else if (*obj).vtable().varsize.is_varsize {

                self.do_chunked_array_start(obj);
            } else {
                // primitive array or no heap pointers
            }
        } else {
            self.do_chunked_array(obj, task.chunk() as _, task.pow() as _);
        }
    }

    unsafe fn do_chunked_array_start(&mut self, obj: *mut HeapObjectHeader) {
        let len = (*obj).array_length();
        ((*obj).vtable().trace)(obj.add(1).cast(), self.visitor);
        if let Some(iter) = (*obj).vtable().varsize.iterate_range {
            if len <= 2048 * 2 {
                // a few slices only, process directly

                iter(obj.add(1).cast(), 0, len, self.visitor);
            } else {
                let mut bits = log2i_graceful(len);

                // Compensate for non-power-of-two arrays, cover the array in excess:
                if len as isize != (1 << bits) {
                    bits += 1;
                }

                // Only allow full chunks on the queue. This frees do_chunked_array() from checking from/to
                // boundaries against array->length(), touching the array header on every chunk.
                //
                // To do this, we cut the prefix in full-sized chunks, and submit them on the queue.
                // If the array is not divided in chunk sizes, then there would be an irregular tail,
                // which we will process separately.

                let mut last_idx = 0;
                let mut chunk = 1;
                let mut pow = bits;
                // Handle overflow
                if pow >= 31 {
                    assert_eq!(pow, 31, "sanity");
                    pow -= 1;

                    chunk = 2;
                    last_idx = 1 << pow;
                    self.visitor
                        .push(MarkTask::new2(obj, true, false, chunk as _, pow as _));
                }

                // Split out tasks, as suggested in ShenandoahMarkTask docs. Record the last
                // successful right boundary to figure out the irregular tail.
                while (1 << pow) > 2048 && (chunk * 2 < MarkTask::chunk_size() as isize) {
                    pow -= 1;
                    let left_chunk = chunk * 2 - 1;
                    let right_chunk = chunk * 2;

                    let left_chunk_end = left_chunk * (1 << pow);

                    if left_chunk_end < len as isize {
                        self.visitor.push(MarkTask::new2(
                            obj,
                            true,
                            false,
                            left_chunk as _,
                            pow as _,
                        ));
                        chunk = right_chunk;
                        last_idx = left_chunk_end;
                    } else {
                        chunk = left_chunk;
                    }
                }
                // Process the irregular tail, if present
                let from = last_idx;
                if from < len as isize {
                    iter(obj.add(1).cast(), from as _, len, self.visitor);
                }
            }
        }
    }

    unsafe fn do_chunked_array(
        &mut self,
        obj: *mut HeapObjectHeader,
        mut chunk: i32,
        mut pow: i32,
    ) {
        let varsize = &(*obj).vtable().varsize;

        while (1 << pow) > 2048 && (chunk * 2 < MarkTask::chunk_size() as i32) {
            pow -= 1;
            chunk *= 2;

            self.visitor.push(MarkTask::new2(
                obj,
                true,
                false,
                chunk as usize - 1,
                pow as _,
            ));
        }

        let chunk_size = 1 << pow;
        let from = (chunk - 1) * chunk_size;
        let to = chunk * chunk_size;

        if let Some(iter) = varsize.iterate_range {
            iter(obj.add(1).cast(), from as _, to as _, self.visitor);
        }
    }
}

const SEGMENT_SIZE: usize = 64;

struct Segment {
    data: Vec<MarkTask>,
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

    fn with(addr: MarkTask) -> Segment {
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

    fn push(&mut self, addr: MarkTask) {
        debug_assert!(self.has_capacity());
        self.data.push(addr);
    }

    fn pop(&mut self) -> Option<MarkTask> {
        self.data.pop()
    }

    fn len(&mut self) -> usize {
        self.data.len()
    }
}

pub struct MarkQueueSet {
    visitors: Vec<*mut SlotVisitor>,
    injector: Arc<Injector<MarkTask>>,
}

impl MarkQueueSet {
    pub fn new(nworkers: usize) -> MarkQueueSet {
        let injector = Arc::new(Injector::new());

        let visitors = (0..nworkers)
            .map(|i| {
                let worker = Worker::new_lifo();
                let stealer = worker.stealer();
                Box::into_raw(Box::new(SlotVisitor::new(
                    i,
                    worker,
                    stealer,
                    injector.clone(),
                )))
            })
            .collect::<Vec<_>>();

        Self { injector, visitors }
    }

    pub fn visitors(&self) -> &[*mut SlotVisitor] {
        &self.visitors
    }

    pub fn visitor(&self, id: usize) -> *mut SlotVisitor {
        self.visitors[id]
    }

    pub fn nworkers(&self) -> usize {
        self.visitors.len()
    }

    pub fn injector(&self) -> &Injector<MarkTask> {
        &self.injector
    }
}

pub struct STWMark {}

impl STWMark {
    pub fn new() -> Self {
        Self {}
    }

    pub fn mark(&self) {
        let heap = heap();

        // push roots
        heap.root_set().process();

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

use crate::utils::*;
cfg_if::cfg_if! {
if #[cfg(target_pointer_width="64")] {

    /// MarkTask
    ///
    /// Encodes both regular oops, and the array oops plus chunking data for parallel array processing.
    /// The design goal is to make the regular obj ops very fast, because that would be the prevailing
    /// case. On the other hand, it should not block parallel array processing from efficiently dividing
    /// the array work.
    ///
    /// The idea is to steal the bits from the 64-bit obj to encode array data, if needed. For the
    /// proper divide-and-conquer strategies, we want to encode the "blocking" data. It turns out, the
    /// most efficient way to do this is to encode the array block as (chunk * 2^pow), where it is assumed
    /// that the block has the size of 2^pow. This requires for pow to have only 5 bits (2^32) to encode
    /// all possible arrays.
    ///
    ///    |xx-------obj---------|-pow-|--chunk---|
    ///    0                    49     54        64
    ///
    /// By definition, chunk == 0 means "no chunk", i.e. chunking starts from 1.
    ///
    /// Lower bits of obj are reserved to handle "skip_live" and "strong" properties. Since this encoding
    /// stores uncompressed oops, those bits are always available. These bits default to zero for "skip_live"
    /// and "weak". This aligns with their frequent values: strong/counted-live references.
    ///
    /// This encoding gives a few interesting benefits:
    ///
    /// a) Encoding/decoding regular oops is very simple, because the upper bits are zero in that task:
    ///
    ///    |---------obj---------|00000|0000000000| /// no chunk data
    ///
    ///    This helps the most ubiquitous path. The initialization amounts to putting the obj into the word
    ///    with zero padding. Testing for "chunkedness" is testing for zero with chunk mask.
    ///
    /// b) Splitting tasks for divide-and-conquer is possible. Suppose we have chunk <C, P> that covers
    /// interval [ (C-1)*2^P; C*2^P ). We can then split it into two chunks:
    ///      <2*C - 1, P-1>, that covers interval [ (2*C - 2)*2^(P-1); (2*C - 1)*2^(P-1) )
    ///      <2*C, P-1>,     that covers interval [ (2*C - 1)*2^(P-1);       2*C*2^(P-1) )
    ///
    ///    Observe that the union of these two intervals is:
    ///      [ (2*C - 2)*2^(P-1); 2*C*2^(P-1) )
    ///
    ///    ...which is the original interval:
    ///      [ (C-1)*2^P; C*2^P )
    ///
    /// c) The divide-and-conquer strategy could even start with chunk <1, round-log2-len(arr)>, and split
    ///    down in the parallel threads, which alleviates the upfront (serial) splitting costs.
    ///
    /// Encoding limitations caused by current bitscales mean:
    ///    10 bits for chunk: max 1024 blocks per array
    ///     5 bits for power: max 2^32 array
    ///    49 bits for   obj: max 512 TB of addressable space
    ///
    /// Stealing bits from obj trims down the addressable space. Stealing too few bits for chunk ID limits
    /// potential parallelism. Stealing too few bits for pow limits the maximum array size that can be handled.
    /// In future, these might be rebalanced to favor one degree of freedom against another. For example,
    /// if/when Arrays 2.0 bring 2^64-sized arrays, we might need to steal another bit for power. We could regain
    /// some bits back if chunks are counted in ObjArrayMarkingStride units.
    ///
    /// There is also a fallback version that uses plain fields, when we don't have enough space to steal the
    /// bits from the native pointer. It is useful to debug the optimized version.
    ///
            #[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Hash, Eq, Ord)]
            pub struct MarkTask {
                /// Everything is encoded into this field...
                obj: usize,
            }


            impl MarkTask {
                pub const CHUNK_BITS: usize = 10;
                pub const POW_BITS: usize = 5;
                pub const OOP_BITS: usize = std::mem::size_of::<usize>() * 8 - Self::CHUNK_BITS - Self::POW_BITS;

                pub const OOP_SHIFT: usize = 0;
                pub const POW_SHIFT: usize = Self::OOP_BITS;
                pub const CHUNK_SHIFT: usize = Self::OOP_BITS + Self::POW_BITS;

                pub const OOP_EXTRACT_MASK: usize = right_nth_bit(Self::OOP_BITS) - 3;
                pub const SKIP_LIVE_EXTRACT_MASK: usize = 1 << 0;
                pub const WEAK_EXTRACT_MASK: usize = 1 << 1;
                pub const CHUNK_POW_EXTRACT_MASK: usize = !right_nth_bit(Self::OOP_BITS);

                pub const CHUNK_RANGE_MASK: usize = right_nth_bit(Self::CHUNK_BITS);
                pub const POW_RANGE_MASK: usize = right_nth_bit(Self::POW_BITS);

                #[inline]
                pub fn decode_oop(val: usize) -> *mut HeapObjectHeader {
                    (val & Self::OOP_EXTRACT_MASK) as _
                }

                #[inline]
                pub fn decode_not_chunked(val: usize) -> bool {
                    (val & Self::CHUNK_POW_EXTRACT_MASK) == 0
                }

                #[inline]
                pub fn decode_chunk(val: usize) -> usize {
                    (val >> Self::CHUNK_SHIFT) & Self::CHUNK_RANGE_MASK
                }

                #[inline]
                pub fn decode_pow(val: usize) -> usize {
                    (val >> Self::POW_SHIFT) & Self::POW_RANGE_MASK
                }

                #[inline]
                pub fn decode_weak(val: usize) -> bool {
                    (val & Self::WEAK_EXTRACT_MASK) != 0
                }

                #[inline]
                pub fn decode_cnt_live(val: usize) -> bool {
                    (val & Self::SKIP_LIVE_EXTRACT_MASK) == 0
                }

                #[inline]
                pub fn encode_oop(obj: *mut HeapObjectHeader, skip_live: bool, weak: bool) -> usize {
                    let mut encoded = obj as usize;
                    if skip_live {
                        encoded |= Self::SKIP_LIVE_EXTRACT_MASK;
                    }

                    if weak {
                        encoded |= Self::WEAK_EXTRACT_MASK;
                    }

                    encoded
                }

                #[inline]
                pub fn encode_chunk(chunk: usize) -> usize {
                    chunk << Self::CHUNK_SHIFT
                }

                #[inline]
                pub fn encode_pow(pow: usize) -> usize {
                    pow << Self::POW_SHIFT
                }

                #[inline]
                pub fn new(obj: *mut HeapObjectHeader, skip_live: bool, weak: bool) -> Self {
                    Self {
                        obj: Self::encode_oop(obj, skip_live, weak)
                    }
                }

                #[inline]
                pub fn new2(obj: *mut HeapObjectHeader, skip_live: bool, weak: bool, chunk: usize, pow: usize) -> Self {
                    let enc_oop = Self::encode_oop(obj, skip_live, weak);
                    let enc_chunk = Self::encode_chunk(chunk);
                    let enc_pow = Self::encode_pow(pow);

                    Self {
                        obj: enc_oop | enc_chunk | enc_pow
                    }
                }

                #[inline]
                pub fn obj(self) -> *mut HeapObjectHeader {
                    Self::decode_oop(self.obj)
                }

                #[inline]
                pub fn chunk(self) -> usize {
                    Self::decode_chunk(self.obj)
                }

                #[inline]
                pub fn pow(self) -> usize {
                    Self::decode_pow(self.obj)
                }

                #[inline]
                pub fn is_not_chunked(self) -> bool {
                    Self::decode_not_chunked(self.obj)
                }

                #[inline]
                pub fn is_weak(self) -> bool {
                    Self::decode_weak(self.obj)
                }

                #[inline]
                pub fn count_liveness(self) -> bool {
                    Self::decode_cnt_live(self.obj)
                }

                #[inline]
                pub fn max_addressable() -> usize {
                    nth_bit(Self::OOP_BITS)
                }

                #[inline]
                pub fn chunk_size() -> usize {
                    nth_bit(Self::CHUNK_BITS)
                }
            }

        } else {
            /// MarkTask
            ///
            /// Encodes both regular oops, and the array oops plus chunking data for parallel array processing.
            /// The design goal is to make the regular obj ops very fast, because that would be the prevailing
            /// case. On the other hand, it should not block parallel array processing from efficiently dividing
            /// the array work.
            ///
            /// The idea is to steal the bits from the 64-bit obj to encode array data, if needed. For the
            /// proper divide-and-conquer strategies, we want to encode the "blocking" data. It turns out, the
            /// most efficient way to do this is to encode the array block as (chunk * 2^pow), where it is assumed
            /// that the block has the size of 2^pow. This requires for pow to have only 5 bits (2^32) to encode
            /// all possible arrays.
            ///
            ///    |xx-------obj---------|-pow-|--chunk---|
            ///    0                    49     54        64
            ///
            /// By definition, chunk == 0 means "no chunk", i.e. chunking starts from 1.
            ///
            /// Lower bits of obj are reserved to handle "skip_live" and "strong" properties. Since this encoding
            /// stores uncompressed oops, those bits are always available. These bits default to zero for "skip_live"
            /// and "weak". This aligns with their frequent values: strong/counted-live references.
            ///
            /// This encoding gives a few interesting benefits:
            ///
            /// a) Encoding/decoding regular oops is very simple, because the upper bits are zero in that task:
            ///
            ///    |---------obj---------|00000|0000000000| /// no chunk data
            ///
            ///    This helps the most ubiquitous path. The initialization amounts to putting the obj into the word
            ///    with zero padding. Testing for "chunkedness" is testing for zero with chunk mask.
            ///
            /// b) Splitting tasks for divide-and-conquer is possible. Suppose we have chunk <C, P> that covers
            /// interval [ (C-1)*2^P; C*2^P ). We can then split it into two chunks:
            ///      <2*C - 1, P-1>, that covers interval [ (2*C - 2)*2^(P-1); (2*C - 1)*2^(P-1) )
            ///      <2*C, P-1>,     that covers interval [ (2*C - 1)*2^(P-1);       2*C*2^(P-1) )
            ///
            ///    Observe that the union of these two intervals is:
            ///      [ (2*C - 2)*2^(P-1); 2*C*2^(P-1) )
            ///
            ///    ...which is the original interval:
            ///      [ (C-1)*2^P; C*2^P )
            ///
            /// c) The divide-and-conquer strategy could even start with chunk <1, round-log2-len(arr)>, and split
            ///    down in the parallel threads, which alleviates the upfront (serial) splitting costs.
            ///
            /// Encoding limitations caused by current bitscales mean:
            ///    10 bits for chunk: max 1024 blocks per array
            ///     5 bits for power: max 2^32 array
            ///    49 bits for   obj: max 512 TB of addressable space
            ///
            /// Stealing bits from obj trims down the addressable space. Stealing too few bits for chunk ID limits
            /// potential parallelism. Stealing too few bits for pow limits the maximum array size that can be handled.
            /// In future, these might be rebalanced to favor one degree of freedom against another. For example,
            /// if/when Arrays 2.0 bring 2^64-sized arrays, we might need to steal another bit for power. We could regain
            /// some bits back if chunks are counted in ObjArrayMarkingStride units.
            ///
            /// There is also a fallback version that uses plain fields, when we don't have enough space to steal the
            /// bits from the native pointer. It is useful to debug the optimized version.
            ///

            #[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Hash, Eq, Ord)]
            pub struct MarkTask {
                obj: usize,
                skip_live: bool,
                weak: bool,
                chunk: i32,
                pow: i32
            }

            impl MarkTask {
                pub const CHUNK_BITS: u8 = 10;
                pub const POW_BITS: u8 = 5;
                pub const CHUNK_MAX: usize = nth_bit(Self::CHUNK_BITS as usize) - 1;
                pub const POW_MAX: usize = nth_bit(Self::POW_BITS as usize) - 1;
            
                #[inline]
                pub fn new(obj: *mut HeapObjectHeader, skip_live: bool, weak: bool) -> Self {
                    Self {
                        obj: obj as usize, 
                        skip_live,
                        weak,
                        chunk: 0,
                        pow: 0
                    }
                }

                #[inline]
                pub fn new2(obj: *mut HeapObjectHeader, skip_live: bool, weak: bool, chunk: usize, pow: usize) -> Self {
                    Self {
                        obj: obj as usize, 
                        skip_live,
                        weak,
                        chunk: chunk as i32,
                        pow: pow as i32
                    }
                }

                #[inline]
                pub fn obj(self) -> *mut HeapObjectHeader {
                    self.obj as *mut HeapObjectHeader
                }

                #[inline]
                pub fn chunk(self) -> usize {
                    self.chunk as usize
                }
                
                #[inline]
                pub fn pow(self) -> usize {
                    self.pow as usize
                }

                #[inline]
                pub fn is_not_chunked(self) -> bool {
                    self.chunk == 0
                }

                #[inline]
                pub fn is_weak(self) -> bool {
                    self.weak
                }
                
                #[inline]
                pub fn count_liveness(self) -> bool {
                    !self.skip_live
                }

                #[inline]
                pub fn max_addressable(self) -> usize {
                    size_of::<usize>()
                }

                #[inline]
                pub fn max_chunk(self) -> usize {
                    Self::CHUNK_MAX
                }
                #[inline]
                pub const fn chunk_size() -> usize {
                    nth_bit(Self::CHUNK_BITS as usize)
                }
            }


        }
}

pub struct GlobalMark {
    visitors: Vec<Arc<SlotVisitor>>,
}

pub struct SlotVisitor {
    task_id: usize,
    heap: *mut Heap,
    mark_ctx: *const MarkingContext,
    local: Segment,
    marked: usize,
    visit_count: usize,
    worker: Worker<MarkTask>,
    stealer: Stealer<MarkTask>,
    injector: Arc<Injector<MarkTask>>,
}

impl SlotVisitor {
    pub fn set_mark_ctx(&mut self, mark_ctx: *const MarkingContext) {
        self.mark_ctx = mark_ctx;
        self.heap = heap();
    }

    pub fn new(
        task_id: usize,
        worker: Worker<MarkTask>,
        stealer: Stealer<MarkTask>,
        injector: Arc<Injector<MarkTask>>,
    ) -> Self {
        Self {
            task_id,
            heap: heap(),
            mark_ctx: null(),
            visit_count: 0,
            worker,
            stealer,
            injector,
            local: Segment::new(),
            marked: 0,
        }
    }

    pub fn visit_count(&self) -> usize {
        self.visit_count
    }

    pub fn mark_ctx(&self) -> &MarkingContext {
        unsafe { &*self.mark_ctx }
    }

    pub fn pop(&mut self) -> Option<MarkTask> {
        self.pop_local()
            .or_else(|| self.pop_worker())
            .or_else(|| self.pop_global())
            .or_else(|| self.steal())
    }

    fn pop_global(&mut self) -> Option<MarkTask> {
        loop {
            let result = self
                .mark_ctx()
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

    fn steal(&self) -> Option<MarkTask> {
        if self.mark_ctx().mark_queues().nworkers() == 1 {
            return None;
        }

        let mut rng = thread_rng();
        let range = Uniform::new(0, self.mark_ctx().mark_queues().nworkers());

        for _ in 0..2 * self.mark_ctx().mark_queues().nworkers() {
            let mut stealer_id = self.task_id;

            while stealer_id == self.task_id {
                stealer_id = range.sample(&mut rng);
            }

            let stealer = unsafe { &(&*self.mark_ctx().mark_queues().visitor(stealer_id)).stealer };

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

    fn pop_local(&mut self) -> Option<MarkTask> {
        if self.local.is_empty() {
            return None;
        }

        let obj = self.local.pop().expect("should be non-empty");
        Some(obj)
    }

    fn pop_worker(&mut self) -> Option<MarkTask> {
        self.worker.pop()
    }

    fn worker(&self) -> &Worker<MarkTask> {
        &self.worker
    }

    fn injector(&self) -> &Injector<MarkTask> {
        &self.injector
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

    pub fn push(&mut self, obj: MarkTask) {
        unsafe {
            debug_assert!((*self.heap).is_in(obj.obj() as _));
        }
        self.visit_count += 1;
        if self.local.has_capacity() {
            self.local.push(obj);
            self.defensive_push();
        } else {
            self.worker().push(obj);
        }
    }

    unsafe fn try_conservative(&mut self, obj: *const u8) {
        if !(*self.heap).is_in(obj as _) {
            return;
        }

        let object = (*self.heap).object_start(obj as _);
        if object.is_null() {
            return;
        }

        self.visit(object.add(1).cast());
    }
}

impl Visitor for SlotVisitor {
    unsafe fn visit(&mut self, obj: *const u8) {
        unsafe {
            let obj = obj.cast::<HeapObjectHeader>().sub(1);
            if self.mark_ctx().mark(obj) {
                if (*obj).should_trace() {
                    self.push(MarkTask::new(obj as _, false, false));
                }
            }
        }
    }

    unsafe fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize) {
        unsafe {
            for i in 0..len {
                let ptr = ptrs.add(i).read();
                self.try_conservative(ptr);
            }
        }
    }

    fn visit_count(&self) -> usize {
        self.visit_count
    }
}
