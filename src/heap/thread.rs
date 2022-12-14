use std::{
    cell::UnsafeCell,
    collections::HashMap,
    intrinsics::{likely, unlikely},
    mem::{size_of, MaybeUninit},
    ptr::{null_mut, null},
    sync::atomic::{AtomicI8, Ordering},
    thread::{JoinHandle, ThreadId},
};

use parking_lot::{Mutex, MutexGuard};

use crate::heap::tlab::ThreadLocalAllocBuffer;
use crate::{
    formatted_size,
    heap::stack::approximate_stack_pointer,
    object::{Allocation, ConstVal, Handle, HeapObjectHeader, SizeTag, VTable, VtableTag, VT},
    traits::Object,
};

use super::{
    align_usize,
    heap::{heap, Heap},
    marking_context::MarkingContext,
    safepoint,
    stack::StackBounds,
    AllocRequest, shared_vars::SharedFlag,
};

// gc_state = 1 means the thread is doing GC or is waiting for the GC to
//              finish.
pub const GC_STATE_WAITING: i8 = 1;
// gc_state = 2 means the thread is running unmanaged code that can be
//              execute at the same time with the GC.
pub const GC_STATE_SAFE: i8 = 2;


/// GC Mutator. 
/// 
/// By using this structure you can allocate, synchronize with GC and insert write barriers.
pub struct ThreadInfo {
    pub(crate) id: ThreadId,
    pub(crate) tlab: ThreadLocalAllocBuffer,
    queue: Box<[*mut HeapObjectHeader]>,
    queue_size: usize,
    queue_index: usize,
    cm_in_progress: *const SharedFlag,
    mark_ctx: *mut MarkingContext,
    stack: StackBounds,
    max_tlab_size: usize,
    safepoint: *mut u8,
    last_sp: *mut u8,
    gc_state: i8,
    safepoint_tasks: HashMap<usize, Box<dyn FnMut()>>,
    free_keys: Vec<usize>,
}

impl ThreadInfo {
    /// Allocates fixed sized object on the heap. 
    #[inline]
    pub fn allocate_fixed<T: 'static + Allocation>(&mut self, value: T) -> Handle<T> {
        unsafe {
            let size = align_usize(T::SIZE + size_of::<HeapObjectHeader>(), 16);
            let mem = self.allocate_raw(size);
            let obj = mem as *mut HeapObjectHeader;
            (*obj).word = 0;
            (*obj).set_vtable(VT::<T>::VAL as *const VTable as _);
            (*obj).set_heap_size(size);
            (*obj).clear_marked();
            (*obj).set_finalization_ordering(false);

            obj.add(1).cast::<T>().write(value);
            // We implement black mutator technique and SATB for concurrent marking. 
            // 
            // This means we collect garbage that was allocated *before* the cycle started
            // and thus all new objects should implicitly be marked as black. This has problem
            // of floating garbage but it significantly simplifies write barrier implementation.
            (*self.mark_ctx).mark(obj as _);
            
            Handle::from_raw(obj.add(1).cast())
        }
    }

    /// Allocates variably sized object on the heap. 
    pub fn allocate_varsize<T: 'static + Allocation>(
        &mut self,
        length: usize,
    ) -> Handle<MaybeUninit<T>> {
        unsafe {
            let size = align_usize(
                T::SIZE + size_of::<HeapObjectHeader>() + T::VARSIZE_ITEM_SIZE * length,
                16,
            );
            let mem = self.allocate_raw(size);
            let obj = mem as *mut HeapObjectHeader;
            (*obj).word = 0;
            (*obj).set_vtable(VT::<T>::VAL as *const VTable as _);
            (*obj).set_heap_size(size);
            (*obj).clear_marked();
            (*obj).set_finalization_ordering(false);

            obj.add(1)
                .cast::<u8>()
                .add(T::VARSIZE_OFFSETOF_LENGTH)
                .cast::<usize>()
                .write(length);
            (*self.mark_ctx).mark(obj as _);
            Handle::from_raw(obj.add(1).cast())
        }
    }

    #[inline]
    pub unsafe fn allocate_raw(&mut self, size: usize) -> *mut u8 {
        let mem = self.alloc_inside_tlab_fast(size);
        if likely(!mem.is_null()) {
            return mem;
        }

        self.allocate_slow(size)
    }

    #[cold]
    #[inline(never)]
    unsafe fn allocate_slow(&mut self, size: usize) -> *mut u8 {
        if size > self.max_tlab_size {
            self.allocate_outside_tlab(size)
        } else {
            let mem = self.alloc_inside_tlab_slow(size);
            if mem.is_null() {
                self.allocate_outside_tlab(size)
            } else {
                mem
            }
        }
    }

    unsafe fn allocate_outside_tlab(&mut self, size: usize) -> *mut u8 {
        let mut req = AllocRequest::new(super::AllocType::Shared, size, size);

        let mem = heap().allocate_memory(&mut req);

        if mem.is_null() {
            std::panic::panic_any(OOM(size));
        }

        mem
    }

    #[inline]
    unsafe fn alloc_inside_tlab_fast(&mut self, size: usize) -> *mut u8 {
        self.tlab.allocate(size)
    }

    unsafe fn alloc_inside_tlab_slow(&mut self, size: usize) -> *mut u8 {
        self.tlab.retire(self.id);

        let tlab_size = self.max_tlab_size;
        let mut req = AllocRequest::new(super::AllocType::ForLAB, tlab_size, tlab_size);
        let mem = heap().allocate_memory(&mut req);

        if mem.is_null() {
            return null_mut();
        }

        std::ptr::write_bytes(mem, 0, req.actual_size());
        self.tlab.initialize_(
            mem as _,
            mem.add(size) as _,
            mem.add(req.actual_size()) as _,
        );
        (*self.tlab.bitmap).set_atomic(mem as _);
        mem
    }

    #[cold]
    pub(crate) fn register(&mut self) {
        self.safepoint = safepoint::SAFEPOINT_PAGE.load(Ordering::Relaxed);
        assert_ne!(self.safepoint, null_mut());
        self.stack = StackBounds::current_thread_stack_bounds();
        self.last_sp = approximate_stack_pointer() as _;
        self.mark_ctx = heap().marking_context_mut() as *mut MarkingContext;
        let th = threads();
        th.get().lock.lock();
        th.get_mut().threads.push(self as *mut ThreadInfo);
        unsafe {
            th.get().lock.unlock();
        }

        for _ in 0..3 {
            self.safepoint();
        }

        let heap = heap();

        self.max_tlab_size = if heap.options().tlab_size > 0 {
            heap.options().tlab_size // TLAB size set by user
        } else {
            heap.options().max_tlab_size // TLAB size set automatically based on heap options
        };

        self.queue = vec![null_mut(); heap.options().max_satb_buffer_size].into_boxed_slice();
        self.queue_index = 0;
        self.queue_size = self.queue.len();
        self.cm_in_progress = &heap.is_concurrent_mark_in_progress;
    }

    
    /// SATB write barrier. Ensures that object processes all references correctly. 
    /// Must be inserted after write to `handle`.
    #[inline]
    pub fn write_barrier<T: Object + ?Sized>(&mut self, handle: Handle<T>) {
        unsafe {
            self.raw_write_barrier(handle.as_ptr().sub(size_of::<HeapObjectHeader>()).cast());
        }

    }
    #[inline]
    pub unsafe fn raw_write_barrier(&mut self, obj: *mut HeapObjectHeader) {
        if !(*self.cm_in_progress).is_set() {
            return;
        }
        
        if !(*self.mark_ctx).is_marked(obj) {
            *self.queue.get_unchecked_mut(self.queue_index) = obj;
            self.queue_index += 1;
            if self.queue_index == self.queue_size {
                self.satb_flush();
            }
        }


    }  
    #[inline(never)]
    #[cold]
    pub fn satb_flush(&mut self) -> bool {
        if self.queue_index == 0 {
            return false;
        }
        let heap = heap();

        for i in 0..self.queue_index {
            unsafe {
                let obj = *self.queue.get_unchecked(i);
                heap.marking_context().mark_queues().injector().push(obj as _);
            }
        }

        self.queue_index = 0;

        true
    }

    pub fn satb_clear(&mut self) {
        self.queue_index = 0;
    }

    pub fn atomic_gc_state(&self) -> &AtomicI8 {
        unsafe { std::mem::transmute(&self.gc_state) }
    }

    #[inline]
    pub(crate) fn gc_state_set(&mut self, state: i8, old_state: i8) -> i8 {
        self.atomic_gc_state().store(state, Ordering::Release);
        if old_state != 0 && state == 0 && !self.safepoint.is_null() {
            self.safepoint();
        }

        old_state
    }

    #[inline]
    pub(crate) fn set_last_sp(&mut self, sp: *mut u8) {
        self.last_sp = sp;
    }

    pub(crate) fn state_save_and_set(&mut self, state: i8) -> i8 {
        self.gc_state_set(state, self.gc_state)
    }

    #[inline]
    pub fn stack_start(&self) -> *mut u8 {
        self.stack.origin
    }

    #[inline]
    pub fn last_sp(&self) -> *mut u8 {
        self.last_sp
    }

    /// Returns pointer to safepoint page. When JITing your code you can directly
    /// inline safepoint poll into your code.
    pub unsafe fn safepoint_page(&self) -> *mut u8 {
        self.safepoint
    }

    /// Returns true if safepoints are conditional in this build of RSGC.
    pub const fn is_conditional_safepoint() -> bool {
        cfg!(feature="conditional-safepoint")
    }
    

    /// Reads from polling page. If safepoint is disabled nothing happens
    /// but when safepoint is enabled this triggers page fault (SIGSEGV/SIGBUS on Linux/macOS/BSD)
    /// and goes into signal to suspend thread.
    /// 
    /// # Note
    /// 
    /// Enable `conditional-safepoint` feature when running in LLDB/GDB, otherwise safepoint events
    /// will be treatened as segfault by debuggers.
    #[inline(always)]
    pub fn safepoint(&mut self) {
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
        let safepoint = self.safepoint;
        let val = unsafe { safepoint.read_volatile() };
        let _ = val;
        #[cfg(feature = "conditional-safepoint")]
        {
            if val != 0 {
                self.enter_conditional();
            }
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }

    #[inline(never)]
    #[cold]
    fn enter_conditional(&mut self) {
        self.enter_safepoint(approximate_stack_pointer() as _);
    }

    /// Sets last stack pointer for a thread, waits for safepoint to be disabled and executes
    /// tasks that are needed to execute at safepoint for this thread.
    pub(crate) fn enter_safepoint(&mut self, sp: *mut u8) {
        let mut start = self.stack.origin;
        let mut end = self.stack.bound;
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }

        assert!(
            sp >= start && sp < end,
            "stack-pointer at safepoint is not in thread stack bounds"
        );
        self.last_sp = sp;

        self.set_gc_and_wait();

        let mut tasks = std::mem::replace(&mut self.safepoint_tasks, HashMap::new());
        for (_, task) in tasks.iter_mut() {
            task();
        }
        self.safepoint_tasks = tasks;
    }

    pub(crate) fn set_gc_and_wait(&mut self) {
        let state = self.gc_state;
        self.atomic_gc_state()
            .store(GC_STATE_WAITING, Ordering::Release);
        super::safepoint::wait_gc();
        self.atomic_gc_state().store(state, Ordering::Release);
    }

    /// Adds tasks that will get executed when thread reaches safepoint.
    ///
    /// # Safety
    ///
    /// `task` should use signal-safe APIs and ideally not access heap APIs.
    pub unsafe fn add_safepoint_task<F: 'static + FnMut()>(&mut self, task: F) -> usize {
        let key = self.free_keys.pop().unwrap_or(self.safepoint_tasks.len());

        self.safepoint_tasks.insert(key, Box::new(task));
        key
    }

    /// Returns true if thread is registered in a GC.
    pub fn is_registered(&self) -> bool {
        !self.safepoint.is_null() && unsafe { self.safepoint != &mut SINK }
    }
}

/// Get [ThreadInfo] reference for allocating memory and accessing GC APIs.
///
/// If thread is not registered by a GC it is first registered and then reference is returned.
pub fn thread() -> &'static mut ThreadInfo {
    unsafe {
        let thread = THREAD.with(|th| th.get());
        if unlikely(!(*thread).is_registered()) {
            (*thread).register();
        }
        &mut *thread
    }
}

/// Acquire [ThreadInfo] reference without registering in a GC.
/// At the moment used only for implementing sync primitives as we do not
/// need to register thread just because it is using our mutex or monitor implementation.
///
/// # Safety
///
/// User must not allocate into thread if it is not registered.
pub unsafe fn thread_no_register() -> &'static mut ThreadInfo {
    THREAD.with(|th| &mut *th.get())
}

pub struct UnsafeScope {
    state: i8,
    thread: &'static mut ThreadInfo,
}

impl UnsafeScope {
    /// Enter unsafe GC state. This means current thread runs "managed by GC code" and GC *must* stop this thread
    /// at GC cycle.
    ///
    /// Returns current state to restore later.
    pub fn new(thread: &'static mut ThreadInfo) -> Self {
        Self {
            state: thread.state_save_and_set(0),
            thread,
        }
    }
}

impl Drop for UnsafeScope {
    fn drop(&mut self) {
        self.thread.gc_state_set(self.state, 0);
    }
}

pub struct SafeScope<'a> {
    state: i8,
    thread: &'a mut ThreadInfo,
}

impl<'a> SafeScope<'a> {
    /// Enter safe GC state. This means current thread runs "unmanaged by GC code" and GC does not need to stop this thread
    /// at GC cycle.
    ///
    /// Returns current state to restore later.
    pub fn new(thread: &'a mut ThreadInfo) -> Self {
        thread.set_last_sp(approximate_stack_pointer() as _);
        Self {
            state: thread.state_save_and_set(GC_STATE_SAFE),
            thread,
        }
    }
}

impl<'a> Drop for SafeScope<'a> {
    fn drop(&mut self) {
        self.thread.gc_state_set(self.state, GC_STATE_SAFE);
    }
}

static mut SINK: u8 = 0;

thread_local! {
    static THREAD: UnsafeCell<ThreadInfo> = UnsafeCell::new(
        ThreadInfo {
            id: std::thread::current().id(),
            mark_ctx: null_mut(),
            tlab: ThreadLocalAllocBuffer::new(),
            stack: StackBounds::current_thread_stack_bounds(),
            safepoint: unsafe { &mut SINK },
            last_sp: null_mut(),
            max_tlab_size: 0,
            gc_state: 0,
            safepoint_tasks: HashMap::new(),
            free_keys: vec![],
            queue: vec![].into_boxed_slice(),
            queue_index: 0,
            queue_size: 0,
            cm_in_progress: null()
        });
}

/*
impl Drop for ThreadInfo {
    fn drop(&mut self) {
        let current = self as *mut Self;
        let safe = SafeScope::new(self);

        acquire_threads().retain(|thread| {
            let thread = *thread;

            thread != current
        });
        drop(safe);
    }
}*/

use parking_lot::{lock_api::RawMutex, RawMutex as Lock};

pub(crate) struct ThreadsInner {
    pub(crate) threads: Vec<*mut ThreadInfo>,
    pub(crate) lock: Lock,
}

pub struct Threads {
    inner: UnsafeCell<ThreadsInner>,
}

impl Threads {
    pub fn new() -> Self {
        Self {
            inner: UnsafeCell::new(ThreadsInner {
                threads: vec![],
                lock: Lock::INIT,
            }),
        }
    }

    pub(crate) fn get(&self) -> &ThreadsInner {
        unsafe { &*self.inner.get() }
    }

    pub(crate) fn get_mut(&self) -> &mut ThreadsInner {
        unsafe { &mut *self.inner.get() }
    }
}

unsafe impl Sync for Threads {}
unsafe impl Send for Threads {}

static THREADS: once_cell::sync::Lazy<Threads> = once_cell::sync::Lazy::new(Threads::new);

pub(crate) fn threads() -> &'static Threads {
    &THREADS
}

pub struct OOM(pub usize);

pub fn spawn<R>(f: impl FnOnce() -> R + Send + 'static) -> JoinHandle<R>
where
    R: Send + 'static,
{
    std::thread::spawn(move || unsafe {
        let thread = thread();

        let res = f();
        thread.tlab.retire(thread.id);
        let current = thread as *mut ThreadInfo;
        let scope = SafeScope::new(thread);
        let threads = threads();
        threads.get().lock.lock();

        threads.get_mut().threads.retain(|thread| {
            let th = *thread;
            if th == current {
                false
            } else {
                true
            }
        });
        threads.get().lock.unlock();
        drop(scope);

        res
    })
}

pub struct ThreadLocalWriteBarrier {
    buffer: Box<[*mut HeapObjectHeader]>,
    size: usize,
    index: usize,
    enabled: bool,
    heap: &'static Heap,
}

impl ThreadLocalWriteBarrier {
    pub fn new(buffer_size: usize) -> Self {
        Self {
            buffer: vec![null_mut(); buffer_size].into_boxed_slice(),
            size: buffer_size,
            index: 0,
            enabled: false,
            heap: heap(),
        }
    }

    #[inline(never)]
    pub unsafe fn write(&mut self, obj: *mut HeapObjectHeader) {
        if self.enabled && !self.heap.marking_context().is_marked(obj) {
            *self.buffer.get_unchecked_mut(self.index) = obj;
            self.index += 1;

            if self.index == self.size {
                self.flush();
            }
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn flush(&mut self) {
        for i in 0..self.index {
            let obj = *self.buffer.get_unchecked(i);
            self.heap.marking_context()
                .mark_queues()
                .injector()
                .push(obj as usize);
        }
        self.index = 0;
    }
}
