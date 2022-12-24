use std::{mem::size_of, sync::atomic::AtomicUsize, ptr::null_mut, ops::{Deref, DerefMut}};

use crate::{
    system::object::HeapObjectHeader,
    utils::{nth_bit, right_nth_bit, taskqueue::*},
};

use super::heap::Heap;

pub struct BufferedOverflowTaskQueue<E: Copy, const N: usize = TASKQUEUE_SIZE> {
    base: OverflowTaskQueue<E, N>,
    elem: Option<E>,
}

impl<E: Copy, const N: usize> BufferedOverflowTaskQueue<E, N> {
    pub fn new() -> Self {
        Self {
            base: OverflowTaskQueue::new(),
            elem: None
        }
    }
    pub fn pop(&mut self) -> Option<E> {
        self.elem
            .take()
            .or_else(|| self.base.pop_local(0).or_else(|| self.base.pop_overflow()))
    }

    pub fn clear(&mut self) {
        self.elem.take();
        self.base.set_empty();
        self.base.overflow_stack().clear(false);
    }
}

impl<E: Copy, const N: usize> TaskQueue for BufferedOverflowTaskQueue<E, N> {
    type E = E;

    fn is_empty(&self) -> bool {
        self.elem.is_none() && self.base.is_empty()
    }

    fn push(&mut self, task: Self::E) -> bool {
        if self.elem.is_none() {
            self.elem = Some(task);
        } else {
            let pushed = (*self.base).push(self.elem.replace(task).unwrap());
            assert!(pushed, "overflow queue should always succeed pushing");
        }

        true
    }

    fn size(&self) -> usize {
        self.base.size()
    }

    fn invalidate_last_queue_id(&mut self) {
        self.base.invalidate_last_queue_id();
    }

    fn last_stolen_queue_id(&self) -> usize {
        self.base.last_stolen_queue_id()
    }

    fn next_random_queue_id(&mut self) -> i32 {
        self.base.next_random_queue_id()
    }

    fn pop_global(&self) -> crate::utils::taskqueue::PopResult<Self::E> {
        self.base.pop_global()
    }

    fn pop_local(&self, threshold: usize) -> Option<Self::E> {
        self.base.pop_local(threshold)
    }

    fn set_last_stolen_queue_id(&mut self, id: usize) {
        self.base.set_last_stolen_queue_id(id)
    }
}

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
            pub const OOP_BITS: usize = size_of::<usize>() * 8 - Self::CHUNK_BITS - Self::POW_BITS;

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
        pub struct MarkTask {

        }
    }
}

pub type ObjToScanQueue = BufferedOverflowTaskQueue<MarkTask>;

pub struct ParallelClaimableQueueSet<T: TaskQueue> {
    base: GenericTaskQueueSet<T>,
    claimed_index: AtomicUsize,
}

impl<T: TaskQueue> ParallelClaimableQueueSet<T> {
    pub fn new(n: usize) -> Self {
        Self {
            base: GenericTaskQueueSet::new(n),
            claimed_index: AtomicUsize::new(0)
        }
    }

    pub fn claim_next(&self) -> *mut T {
        let size = self.base.size();

        if self.claimed_index.load(atomic::Ordering::Relaxed) >= size {
            return null_mut();
        }

        let index = self.claimed_index.fetch_add(1, atomic::Ordering::Relaxed);

        if index < size {
            return self.base.queue(index);
        } else {
            return null_mut()
        }
    }

    pub fn clear_claimed(&self) {
        self.claimed_index.store(0, atomic::Ordering::Relaxed);
    }
}

impl<T: TaskQueue> TaskQueueSetSuper for ParallelClaimableQueueSet<T> {
    fn size(&self) -> usize {
        self.base.size()
    }

    fn tasks(&self) -> usize {
        self.base.tasks()
    }
}

impl<T: TaskQueue> TaskQueueSetSuperImpl<T> for ParallelClaimableQueueSet<T> {
    fn queue(&self, queue_num: usize) -> *mut T {
        self.base.queue(queue_num)
    }

    fn register_queue(&self, i: usize, queue: *mut T) {
        self.base.register_queue(i, queue)
    }

    fn steal(&self, queue_num: usize) -> Option<<T as TaskQueue>::E> {
        self.base.steal(queue_num)
    }

    fn steal_best_of_2(&self, queue_num: usize) -> PopResult<<T as TaskQueue>::E> {
        self.base.steal_best_of_2(queue_num)
    }
}

impl<T: TaskQueue> Deref for ParallelClaimableQueueSet<T> {
    type Target = GenericTaskQueueSet<T>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl<T: TaskQueue> DerefMut for ParallelClaimableQueueSet<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}


pub type ObjToScanQueueSet = ParallelClaimableQueueSet<ObjToScanQueue>;

impl ObjToScanQueueSet {
    pub fn clear(&self) {
        let size = self.base.size();

        for index in 0..size {
            let q = self.base.queue(index);
            unsafe {
                (*q).clear();
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        let size = self.base.size();

        for index in 0..size {
            let q = self.base.queue(index);
            unsafe {
                if !(*q).is_empty() {
                    return false;
                }
            }
        }

        true
    }
}

pub struct GCTerminator<'a> {
    heap: &'a Heap
}

impl<'a> GCTerminator<'a> {
    pub fn new(heap: &'a Heap) -> Self {
        Self {
            heap
        }
    }
}

impl<'a> TerminatorTerminator for GCTerminator<'a> {
    fn should_exit_termination(&self) -> bool {
        self.heap.cancelled_gc()
    }
}

