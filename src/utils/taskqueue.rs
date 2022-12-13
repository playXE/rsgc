use atomic::{Atomic, Ordering};
use std::{
    cell::UnsafeCell,
    mem::size_of,
    ops::{Deref, DerefMut},
    sync::atomic::AtomicUsize,
};
use crate::heap::*;
use crate::{utils::stack::Stack, object::HeapObjectHeader};

#[cfg(target_pointer_width = "64")]
pub type Idx = u32;
#[cfg(target_pointer_width = "32")]
pub type Idx = u16;

#[cfg(target_pointer_width = "64")]
pub const TASKQUEUE_SIZE: usize = 1 << 17;
#[cfg(target_pointer_width = "32")]
pub const TASKQUEUE_SIZE: usize = 1 << 14;

pub union Age {
    flags: (Idx, Idx),
    data: usize,
}

impl Age {
    pub fn new_data(data: usize) -> Self {
        Self { data }
    }

    pub fn new_flags(top: Idx, tag: Idx) -> Self {
        Self { flags: (top, tag) }
    }

    pub fn top(&self) -> Idx {
        unsafe { self.flags.0 }
    }

    pub fn tag(&self) -> Idx {
        unsafe { self.flags.1 }
    }
}

impl PartialEq for Age {
    fn eq(&self, other: &Self) -> bool {
        unsafe { self.data == other.data }
    }
}

impl Eq for Age {}

impl Clone for Age {
    fn clone(&self) -> Self {
        *self
    }
}
impl Copy for Age {}

pub struct TaskQueueSuper<const N: usize> {
    bottom: AtomicUsize,
    age: Age,
}

impl<const N: usize> TaskQueueSuper<N> {
    /// Maximum number of elements allowed in the queue.  This is two less
    /// than the actual queue size, so that a full queue can be distinguished
    /// from underflow involving pop_local and concurrent pop_global operations
    /// in GenericTaskQueue.
    pub const fn max_elems() -> usize {
        N - 2
    }

    pub const MOD_N_MASK: usize = N - 1;

    pub const fn dirty_size(bot: usize, top: usize) -> usize {
        (bot.wrapping_sub(top)) & Self::MOD_N_MASK
    }

    pub const fn clean_size(bot: usize, top: usize) -> usize {
        let sz = Self::dirty_size(bot, top);

        // Has the queue "wrapped", so that bottom is less than top?  There's a
        // complicated special case here.  A pair of threads could perform pop_local
        // and pop_global operations concurrently, starting from a state in which
        // _bottom == _top+1.  The pop_local could succeed in decrementing _bottom,
        // and the pop_global in incrementing _top (in which case the pop_global
        // will be awarded the contested queue element.)  The resulting state must
        // be interpreted as an empty queue.  (We only need to worry about one such
        // event: only the queue owner performs pop_local's, and several concurrent
        // threads attempting to perform the pop_global will all perform the same
        // CAS, and only one can succeed.)  Any stealing thread that reads after
        // either the increment or decrement will see an empty queue, and will not
        // join the competitors.  The "sz == -1" / "sz == N-1" state will not be
        // modified by concurrent threads, so the owner thread can reset the state
        // to _bottom == top so subsequent pushes will be performed normally.
        if sz == N - 1 {
            0
        } else {
            sz
        }
    }

    pub const fn assert_not_underflow(dirty_size: usize) {
        assert!(dirty_size != N - 1, "invariant");
    }

    pub const fn increment_index(ind: usize) -> usize {
        (ind + 1) & Self::MOD_N_MASK
    }

    pub const fn decrement_index(ind: usize) -> usize {
        (ind.wrapping_sub(1)) & Self::MOD_N_MASK
    }

    pub fn age_top_relaxed(&self) -> Idx {
        unsafe { atomic_load(&self.age.flags.0, Ordering::Relaxed) }
    }

    pub fn cmpxchg_age(&self, old_age: Age, new_age: Age) -> Age {
        atomic_cmpxchg(&self.age, old_age, new_age)
    }

    pub fn set_age_relaxed(&self, new_age: Age) {
        atomic_store(&self.age, new_age, Ordering::Relaxed);
    }

    pub fn release_set_bottom(&self, new_bottom: usize) {
        self.bottom.store(new_bottom, Ordering::Release);
    }

    pub fn set_bottom_relaxed(&self, new_bottom: usize) {
        self.bottom.store(new_bottom, Ordering::Relaxed);
    }

    pub fn bottom_acquire(&self) -> usize {
        self.bottom.load(Ordering::Acquire)
    }

    pub fn bottom_relaxed(&self) -> usize {
        self.bottom.load(Ordering::Relaxed)
    }

    pub const fn new() -> Self {
        Self {
            bottom: AtomicUsize::new(0),
            age: Age { data: 0 },
        }
    }

    pub fn assert_empty(&self) {
        assert!(
            self.bottom_relaxed() == self.age_top_relaxed() as _,
            "not empty"
        )
    }

    pub fn is_empty(&self) -> bool {
        self.size() == 0
    }

    /// Return an estimate of the number of elements in the queue.
    /// Treats pop_local/pop_global race that underflows as empty.
    pub fn size(&self) -> usize {
        Self::clean_size(self.bottom_relaxed(), self.age_top_relaxed() as _)
    }

    pub fn set_empty(&self) {
        self.set_bottom_relaxed(0);
        self.set_age_relaxed(Age { data: 0 });
    }

    pub fn age_relaxed(&self) -> Age {
        {
            atomic_load(&self.age, Ordering::Relaxed)
        }
    }
}

pub enum PopResult<T> {
    Empty,
    Contended,
    Success(T),
}

pub trait TaskQueue {
    type E;
    fn push(&mut self, task: Self::E) -> bool;
    fn pop_local(&self, threshold: usize) -> Option<Self::E>;
    fn pop_global(&self) -> PopResult<Self::E>;
    fn size(&self) -> usize;

    fn invalidate_last_queue_id(&mut self);
    fn last_stolen_queue_id(&self) -> usize;
    fn set_last_stolen_queue_id(&mut self, id: usize);
    fn next_random_queue_id(&mut self) -> i32;

    fn is_empty(&self) -> bool;
}

///
/// GenericTaskQueue implements an ABP, Aurora-Blumofe-Plaxton, double-
/// ended-queue (deque), intended for use in work stealing. Queue operations
/// are non-blocking.
///
/// A queue owner thread performs push() and pop_local() operations on one end
/// of the queue, while other threads may steal work using the pop_global()
/// method.
///
/// The main difference to the original algorithm is that this
/// implementation allows wrap-around at the end of its allocated
/// storage, which is an array.
///
/// The original paper is:
///
/// Arora, N. S., Blumofe, R. D., and Plaxton, C. G.
/// Thread scheduling for multiprogrammed multiprocessors.
/// Theory of Computing Systems 34, 2 (2001), 115-144.
///
/// The following paper provides an correctness proof and an
/// implementation for weakly ordered memory models including (pseudo-)
/// code containing memory barriers for a Chase-Lev deque. Chase-Lev is
/// similar to ABP, with the main difference that it allows resizing of the
/// underlying storage:
///
/// Le, N. M., Pop, A., Cohen A., and Nardell, F. Z.
/// Correct and efficient work-stealing for weak memory models
/// Proceedings of the 18th ACM SIGPLAN symposium on Principles and
/// practice of parallel programming (PPoPP 2013), 69-80
///
#[repr(C)]
pub struct GenericTaskQueue<E, const N: usize = TASKQUEUE_SIZE> {
    base: TaskQueueSuper<N>,
    elems: *mut E,
    last_stolen_queue_id: usize,
    seed: i32,
}

impl<E, const N: usize> GenericTaskQueue<E, N> {
    pub const INVALID_QUEUE_ID: usize = usize::MAX;

    pub fn new() -> Self {
        unsafe {
            let elems = libc::malloc(size_of::<E>() * N) as *mut E;

            Self {
                base: TaskQueueSuper::new(),
                elems,
                last_stolen_queue_id: Self::INVALID_QUEUE_ID,
                seed: 17, /* random number */
            }
        }
    }

    pub fn set_last_stolen_queue_id(&mut self, id: usize) {
        self.last_stolen_queue_id = id;
    }

    pub fn last_stolen_queue_id(&self) -> usize {
        self.last_stolen_queue_id
    }

    pub fn is_last_stolen_queue_id_valid(&self) -> bool {
        self.last_stolen_queue_id != Self::INVALID_QUEUE_ID
    }

    pub fn invalidate_last_queue_id(&mut self) {
        self.last_stolen_queue_id = Self::INVALID_QUEUE_ID;
    }

    /// pop_local_slow() is done by the owning thread and is trying to
    /// get the last task in the queue.  It will compete with pop_global()
    /// that will be used by other threads.  The tag age is incremented
    /// whenever the queue goes empty which it will do here if this thread
    /// gets the last task or in pop_global() if the queue wraps (top == 0
    /// and pop_global() succeeds, see pop_global()).
    #[cold]
    fn pop_local_slow(&self, local_bot: usize, old_age: Age) -> bool {
        // This queue was observed to contain exactly one element; either this
        // thread will claim it, or a competing "pop_global".  In either case,
        // the queue will be logically empty afterwards.  Create a new Age value
        // that represents the empty queue for the given value of "bottom".  (We
        // must also increment "tag" because of the case where "bottom == 1",
        // "top == 0".  A pop_global could read the queue element in that case,
        // then have the owner thread do a pop followed by another push.  Without
        // the incrementing of "tag", the pop_global's CAS could succeed,
        // allowing it to believe it has claimed the stale element.)
        let new_age = Age::new_flags(local_bot as _, old_age.tag() + 1);

        // Perhaps a competing pop_global has already incremented "top", in which
        // case it wins the element.
        if local_bot == old_age.top() as usize {
            // No competing pop_global has yet incremented "top"; we'll try to
            // install new_age, thus claiming the element.
            let temp_age = self.cmpxchg_age(old_age, new_age);

            if temp_age == old_age {
                // we win
                return true;
            }
        }

        // We lose; a competing pop_global got the element.  But the queue is empty
        // and top is greater than bottom.  Fix this representation of the empty queue
        // to become the canonical one.
        self.set_age_relaxed(new_age);
        false
    }
}

impl<E, const N: usize> TaskQueue for GenericTaskQueue<E, N> {
    type E = E;
    fn size(&self) -> usize {
        (**self).size()
    }

    fn is_empty(&self) -> bool {
        self.base.is_empty()
    }
    fn push(&mut self, t: E) -> bool {
        let local_bot = self.bottom_relaxed();
        assert!(local_bot < N, "bottom out of range");
        let top = self.age_top_relaxed();
        let dirty_n_elems = TaskQueueSuper::<N>::dirty_size(local_bot, top as _);

        // A dirty_size of N-1 cannot happen in push.  Considering only push:
        // (1) dirty_n_elems is initially 0.
        // (2) push adds an element iff dirty_n_elems < max_elems(), which is N - 2.
        // (3) only push adding an element can increase dirty_n_elems.
        // => dirty_n_elems <= N - 2, by induction
        // => dirty_n_elems < N - 1, invariant
        //
        // A pop_global that is concurrent with push cannot produce a state where
        // dirty_size == N-1.  pop_global only removes an element if dirty_elems > 0,
        // so can't underflow to -1 (== N-1) with push.
        assert!(
            dirty_n_elems <= TaskQueueSuper::<N>::max_elems(),
            "n_elems out of range"
        );

        if dirty_n_elems < TaskQueueSuper::<N>::max_elems() {
            unsafe {
                self.elems.add(local_bot).write(t);
            }
            self.release_set_bottom(TaskQueueSuper::<N>::increment_index(local_bot));
            true
        } else {
            false
        }
    }

    fn pop_local(&self, threshold: usize) -> Option<E> {
        let mut local_bot = self.bottom_relaxed();
        // This value cannot be N-1.  That can only occur as a result of
        // the assignment to bottom in this method.  If it does, this method
        // resets the size to 0 before the next call (which is sequential,
        // since this is pop_local.)
        let dirty_n_elems = TaskQueueSuper::<N>::dirty_size(local_bot, self.age_top_relaxed() as _);

        if dirty_n_elems <= threshold {
            return None;
        }

        local_bot = TaskQueueSuper::<N>::decrement_index(local_bot);
        self.set_bottom_relaxed(local_bot);

        atomic::fence(Ordering::Acquire);

        unsafe {
            let t = self.elems.add(local_bot);

            let tp = self.age_top_relaxed();

            if TaskQueueSuper::<N>::clean_size(local_bot, tp as _) > 0 {
                return Some(t.read());
            } else {
                let res = self.pop_local_slow(local_bot, self.age_relaxed());
                if res {
                    return Some(t.read());
                } else {
                    return None;
                }
            }
        }
    }

    fn pop_global(&self) -> PopResult<E> {
        let old_age = self.age_relaxed();
        // Architectures with non-multi-copy-atomic memory model require a
        // full fence here to guarantee that bottom is not older than age,
        // which is crucial for the correctness of the algorithm.
        //
        // We need a full fence here for this case:
        //
        // Thread1: set bottom (push)
        // Thread2: read age, read bottom, set age (pop_global)
        // Thread3: read age, read bottom (pop_global)
        //
        // The requirement is that Thread3 must never read an older bottom
        // value than Thread2 after Thread3 has seen the age value from
        // Thread2.
        atomic::fence(Ordering::Acquire);

        let local_bot = self.bottom_acquire();
        let n_elems = TaskQueueSuper::<N>::clean_size(local_bot, old_age.top() as _);

        if n_elems == 0 {
            return PopResult::Empty;
        }

        unsafe {
            let t = self.elems.add(old_age.top() as _);

            let new_top = TaskQueueSuper::<N>::increment_index(old_age.top() as _);
            let new_tag = old_age.tag() + if new_top == 0 { 1 } else { 0 };

            let new_age = Age::new_flags(new_top as _, new_tag);
            let res_age = self.cmpxchg_age(old_age, new_age);

            if res_age == old_age {
                return PopResult::Success(t.read());
            } else {
                return PopResult::Contended;
            }
        }
    }
    
    fn last_stolen_queue_id(&self) -> usize {
        self.last_stolen_queue_id
    }

    fn invalidate_last_queue_id(&mut self) {
        self.last_stolen_queue_id = usize::MAX;
    }

    fn set_last_stolen_queue_id(&mut self, id: usize) {
        self.last_stolen_queue_id = id;
    }

    fn next_random_queue_id(&mut self) -> i32 {
        random_park_and_miller(&mut self.seed)
    }
}

impl<E, const N: usize> Drop for GenericTaskQueue<E, N> {
    fn drop(&mut self) {
        unsafe {
            libc::free(self.elems as *mut libc::c_void);
        }
    }
}

impl<E, const N: usize> Deref for GenericTaskQueue<E, N> {
    type Target = TaskQueueSuper<N>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

/// OverflowTaskQueue is a TaskQueue that also includes an overflow stack for
/// elements that do not fit in the TaskQueue.
///
/// This class hides two methods from super classes:
///
/// push() - push onto the task queue or, if that fails, onto the overflow stack
/// is_empty() - return true if both the TaskQueue and overflow stack are empty
///
/// Note that size() is not hidden--it returns the number of elements in the
/// TaskQueue, and does not include the size of the overflow stack.  This
/// simplifies replacement of GenericTaskQueues with OverflowTaskQueues.
pub struct OverflowTaskQueue<E: Copy, const N: usize = TASKQUEUE_SIZE> {
    base: GenericTaskQueue<E, N>,
    overflow: UnsafeCell<Stack<E>>,
}

impl<E: Copy, const N: usize> Deref for OverflowTaskQueue<E, N> {
    type Target = GenericTaskQueue<E, N>;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl<E: Copy, const N: usize> DerefMut for OverflowTaskQueue<E, N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl<E: Copy, const N: usize> OverflowTaskQueue<E, N> {
    pub fn new() -> Self {
        Self {
            base: GenericTaskQueue::new(),
            overflow: UnsafeCell::new(Stack::new(Stack::<E>::DEFAULT_SEGMENT_SIZE, 4, 0)),
        }
    }

    pub fn taskqueue_empty(&self) -> bool {
        self.base.is_empty()
    }

    pub fn overflow_empty(&self) -> bool {
        self.overflow_stack().is_empty()
    }

    pub fn overflow_stack(&self) -> &mut Stack<E> {
        unsafe { &mut *self.overflow.get() }
    }

    pub fn try_push_to_taskqueue(&mut self, elem: E) -> bool {
        self.base.push(elem)
    }

    pub fn pop_overflow(&mut self) -> Option<E> {
        self.overflow_stack().pop()
    }
}

impl<E: Copy, const N: usize> TaskQueue for OverflowTaskQueue<E, N> {
    type E = E;

    fn is_empty(&self) -> bool {
        self.taskqueue_empty() && self.overflow_empty()
    }

    fn size(&self) -> usize {
        self.base.size()
    }

    fn push(&mut self, elem: E) -> bool {
        if !self.base.push(elem) {
            self.overflow_stack().push(elem);
        }
        true
    }

    fn pop_global(&self) -> PopResult<E> {
        self.base.pop_global()
    }

    fn pop_local(&self, threshold: usize) -> Option<E> {
        self.base.pop_local(threshold)
    }

    fn invalidate_last_queue_id(&mut self) {
        self.base.invalidate_last_queue_id();
    }

    fn last_stolen_queue_id(&self) -> usize {
        self.base.last_stolen_queue_id
    }

    fn next_random_queue_id(&mut self) -> i32 {
        self.base.next_random_queue_id()
    }

    fn set_last_stolen_queue_id(&mut self, id: usize) {
        self.base.set_last_stolen_queue_id(id);
    }
}

pub trait TaskQueueSetSuper {
    fn tasks(&self) -> usize;
    fn size(&self) -> usize;
}

pub trait TaskQueueSetSuperImpl<T: TaskQueue>: TaskQueueSetSuper {
    fn steal_best_of_2(&self, queue_num: usize) -> PopResult<T::E>;

    fn queue(&self, queue_num: usize) -> *mut T;
    fn steal(&self, queue_num: usize) -> Option<T::E>;
    fn register_queue(&self, i: usize, queue: *mut T);
}

pub struct GenericTaskQueueSet<T: TaskQueue> {
    queues: *mut *mut T,
    n: usize,
}
impl<T: TaskQueue> GenericTaskQueueSet<T> {
    pub fn new(n: usize) -> Self {
        let queues = unsafe { libc::malloc(n * std::mem::size_of::<*mut T>()) as *mut *mut T };

        for i in 0..n {
            unsafe {
                *queues.add(i) = std::ptr::null_mut();
            }
        }
        Self { queues, n }
    }
}

impl<T: TaskQueue> Drop for GenericTaskQueueSet<T> {
    fn drop(&mut self) {
        unsafe {
            libc::free(self.queues as *mut libc::c_void);
        }
    }
}

impl<T: TaskQueue> TaskQueueSetSuper for GenericTaskQueueSet<T> {
    fn tasks(&self) -> usize {
        let mut n = 0;
        for j in 0..self.n {
            n += unsafe { (&**(&*self.queues.add(j))).size() }
        }

        n
    }

    fn size(&self) -> usize {
        self.n
    }
}

#[inline]
fn random_park_and_miller(seed0: &mut i32) -> i32 {
    let a = 16807;
    let m = 2147483647;
    let q = 127773; /* m div a */
    let r = 2836; /* m mod a */

    let mut seed = *seed0;
    let hi = seed / q;
    let lo = seed % q;
    let test = a * lo - r * hi;
    if test > 0 {
        seed = test;
    } else {
        seed = test + m;
    }
    *seed0 = seed;
    return seed;
}

impl<T: TaskQueue> TaskQueueSetSuperImpl<T> for GenericTaskQueueSet<T> {
    fn register_queue(&self, i: usize, queue: *mut T) {
        unsafe {
            self.queues.add(i).write(queue);
        }
    }

    fn queue(&self, queue_num: usize) -> *mut T {
        unsafe { self.queues.add(queue_num).read() }
    }

    fn steal_best_of_2(&self, queue_num: usize) -> PopResult<T::E> {
        unsafe {
            let local_queue = self.queue(queue_num);
            if self.n > 2 {
                let mut k1 = queue_num;

                if (*local_queue).last_stolen_queue_id() != usize::MAX {
                    k1 = (*local_queue).last_stolen_queue_id();
                } else {
                    while k1 == queue_num {
                        k1 = (*local_queue).next_random_queue_id() as usize % self.n;
                    }
                }

                let mut k2 = queue_num;

                while k2 == queue_num || k2 == k1 {
                    k2 = (*local_queue).next_random_queue_id() as usize % self.n;
                }

                let sz1 = (*self.queue(k1)).size();
                let sz2 = (*self.queue(k2)).size();
                let sel_k;

                let suc;

                if sz2 > sz1 {
                    sel_k = k2;
                    suc = (*self.queue(k2)).pop_global();
                } else {
                    sel_k = k1;
                    suc = (*self.queue(k1)).pop_global();
                }

                if let PopResult::Success(_) = suc {
                    (*local_queue).set_last_stolen_queue_id(sel_k);
                } else {
                    (*local_queue).invalidate_last_queue_id();
                }

                return suc;
            } else if self.n == 2 {
                let k = (queue_num + 1) % 2;
                return (*self.queue(k)).pop_global()
            } else {
                return PopResult::Empty;
            }
        }
    }

    fn steal(&self, queue_num: usize) -> Option<T::E> {
        let num_retries = 2 * self.n;

        for _ in 0..num_retries {
            let sr = self.steal_best_of_2(queue_num);

            if let PopResult::Success(val) = sr {
                return Some(val);
            } else if let PopResult::Contended = sr {
                continue;
            } else {
                continue;
            }
        }

        None
    }
}


#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ObjArrayTask {
    obj: *mut HeapObjectHeader,
    idx: usize 
}

impl ObjArrayTask {
    #[inline]
    pub const fn obj(&self) -> *mut HeapObjectHeader {
        self.obj
    }

    #[inline]
    pub const fn idx(&self) -> usize {
        self.idx
    }

    #[inline]
    pub const fn new(obj: *mut HeapObjectHeader, idx: usize) -> Self {
        Self { obj, idx }
    }
}


/// Wrapper over an obj that is a partially scanned array.
/// Can be converted to a ScannerTask for placement in associated task queues.
/// Refers to the partially copied source array obj.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PartialArrayScanTask {
    src: *mut HeapObjectHeader
}

impl PartialArrayScanTask {
    #[inline]
    pub const fn src(&self) -> *mut HeapObjectHeader {
        self.src
    }

    #[inline]
    pub const fn new(src: *mut HeapObjectHeader) -> Self {
        Self { src }
    }
}

// Discriminated union over HeapObjectHeader* and PartialArrayScanTask.
// Uses a low tag in the associated pointer to identify the category.
// Used as a task queue element type.
pub struct ScannerTask {
    p: *mut u8 
}

impl ScannerTask {
    pub const OOP_TAG: usize = 0;
    pub const PARTIAL_ARRAY_TAG: usize = 1;
    pub const TAG_SIZE: usize = 1;
    pub const TAG_ALIGNMENT: usize = 1 << Self::TAG_SIZE;
    pub const TAG_MASK: usize = Self::TAG_ALIGNMENT - 1;

    pub fn encode(obj: *mut HeapObjectHeader, tag: usize) -> Self {
        Self { p: (obj as usize + tag) as *mut u8 }
    }

    pub fn raw_value(&self) -> *mut u8 {
        self.p
    }

    pub fn has_tag(&self, tag: usize) -> bool {
        (self.p as usize & Self::TAG_MASK) == tag
    }

    pub fn decode(&self, tag: usize) -> *mut HeapObjectHeader {
        (self.p as usize - tag) as *mut HeapObjectHeader
    }

    pub fn new(obj: *mut HeapObjectHeader) -> Self {
        Self::encode(obj, Self::OOP_TAG)
    }

    pub fn partial_array(obj: PartialArrayScanTask) -> Self {
        Self::encode(obj.src(), Self::PARTIAL_ARRAY_TAG)
    }

    pub fn is_oop_ptr(&self) -> bool {
        self.has_tag(Self::OOP_TAG)
    }

    pub fn is_partial_array_ptr(&self) -> bool {
        self.has_tag(Self::PARTIAL_ARRAY_TAG)
    }

    pub fn to_oop_ptr(&self) -> *mut HeapObjectHeader {
        self.decode(Self::OOP_TAG)
    }

    pub fn to_partial_array_task(&self) -> PartialArrayScanTask {
        PartialArrayScanTask::new(self.decode(Self::PARTIAL_ARRAY_TAG))
    }
}


/// When to terminate from the termination protocol.
pub trait TerminatorTerminator {
    fn should_exit_termination(&self) -> bool;
}