use std::{
    any::Any,
    cell::UnsafeCell,
    collections::HashMap,
    intrinsics::{likely, unlikely},
    mem::{size_of, MaybeUninit},
    panic::{AssertUnwindSafe, UnwindSafe},
    ptr::{null, null_mut},
    sync::atomic::{AtomicI8, AtomicU64, AtomicUsize, Ordering},
    thread::{JoinHandle, ThreadId},
};

use crate::{heap::tlab::ThreadLocalAllocBuffer, system::finalizer::register_for_finalization};
use crate::{
    formatted_size,
    heap::{align_down, mark::MarkTask, stack::approximate_stack_pointer},
    offsetof,
    sync::{
        self,
        mutex::{Condvar, Mutex, MutexGuard},
    },
    system::object::{
        Allocation, ConstVal, Handle, HeapObjectHeader, SizeTag, VTable, VtableTag, VT,
    },
    system::traits::Object,
    utils::{
        deque::LocalSSB,
        machine_context::{registers_from_ucontext, PlatformRegisters},
    },
};

use super::{
    align_usize,
    bitmap::HeapBitmap,
    card_table::CardTable,
    heap::{heap, Heap},
    marking_context::MarkingContext,
    region::HeapArguments,
    safepoint,
    shared_vars::SharedFlag,
    stack::StackBounds,
    AllocRequest,
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
pub struct Thread {
    pub(crate) id: u64,
    pub(crate) tlab: ThreadLocalAllocBuffer,
    pub(crate) biased_begin: usize,
    satb_mark_queue: LocalSSB,
    cm_in_progress: bool,
    mark_ctx: *mut MarkingContext,
    mark_bitmap: *const HeapBitmap<16>,
    stack: StackBounds,
    max_tlab_size: usize,
    safepoint: *mut u8,
    last_sp: *mut u8,
    gc_state: i8,
    pub(crate) platform_registers: *mut PlatformRegisters,
}

static THREAD_ID: AtomicU64 = AtomicU64::new(0);

impl Thread {
    pub fn safepoint_offset() -> usize {
        offsetof!(Thread, safepoint)
    }

    pub fn mark_queue_offset() -> usize {
        offsetof!(Thread, satb_mark_queue)
    }

    pub fn satb_buffer_offset() -> usize {
        offsetof!(Thread, satb_mark_queue.buf)
    }

    pub fn satb_index_offset() -> usize {
        offsetof!(Thread, satb_mark_queue.index)
    }

    pub fn cm_in_progress_offset() -> usize {
        offsetof!(Thread, cm_in_progress)
    }

    pub fn mark_ctx_offset() -> usize {
        offsetof!(Thread, mark_ctx)
    }

    pub fn mark_bitmap_offset() -> usize {
        offsetof!(Thread, mark_bitmap)
    }

    pub fn tlab_start_offset() -> usize {
        offsetof!(Thread, tlab.start)
    }

    pub fn tlab_top_offset() -> usize {
        offsetof!(Thread, tlab.top)
    }

    pub fn tlab_end_offset() -> usize {
        offsetof!(Thread, tlab.end)
    }

    pub fn tlab_bitmap_offset() -> usize {
        offsetof!(Thread, tlab.bitmap)
    }

    pub unsafe fn satb_mark_queue(&self) -> &LocalSSB {
        &self.satb_mark_queue
    }

    pub unsafe fn satb_mark_queue_mut(&mut self) -> &mut LocalSSB {
        &mut self.satb_mark_queue
    }

    /// Allocates fixed sized object on the heap.
    pub fn allocate<T: 'static + Allocation>(&mut self, value: T) -> Handle<T> {
        unsafe {
            let size = align_usize(T::SIZE + size_of::<HeapObjectHeader>(), 16);
            let mem = self.allocate_raw(size);
            let obj = mem as *mut HeapObjectHeader;
            (*obj).word = 0;
            (*obj).set_vtable(VT::<T>::VAL as *const VTable as _);
            (*obj).set_heap_size(size);
            if T::NO_HEAP_PTRS {
                (*obj).set_no_heap_ptrs();
            }
            obj.add(1).cast::<T>().write(value);

            // We implement black mutator technique and SATB for concurrent marking.
            //
            // This means we collect garbage that was allocated *before* the cycle started
            // and thus all new objects should implicitly be marked as black. This has problem
            // of floating garbage but it significantly simplifies write barrier implementation.

            #[cfg(feature = "gc-satb")]
            {
                // Need to maintain black-mutator invariant with SATB
                (*self.mark_bitmap).set_bit(obj as _);
            }   

            let handle = Handle::from_raw(obj.add(1).cast());

            if T::FINALIZE {
                register_for_finalization(handle);
            }

            handle

            
        }
    }

    /// Allocates variably sized object on the heap.
    ///
    /// Note that `length` field is automatically written at [Allocation::VARSIZE_OFFSETOF_CAPACITY].
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
            if T::NO_HEAP_PTRS {
                (*obj).set_no_heap_ptrs();
            }
            obj.add(1)
                .cast::<u8>()
                .add(T::VARSIZE_OFFSETOF_CAPACITY)
                .cast::<usize>()
                .write(length);

            #[cfg(feature = "gc-satb")]
            {
                // Need to maintain black-mutator invariant with SATB
                (*self.mark_bitmap).set_bit(obj as _);
            }
            
            let handle = Handle::from_raw(obj.add(1).cast());

            if T::FINALIZE {
                register_for_finalization(handle);
            }

            handle
        }
    }

    /// Allocates raw memory, unsafe to use outside of RSGC impl itself.
    ///
    /// ## TODO
    ///
    /// I should really update it to include all code to properly tell GC that we allocated something, right now it is done in `allocate` and `allocate_varsize` but it should be done here.
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
        assert!(
            self.is_registered(),
            "trying to perform allocation in unregistered thread with id: {:?}",
            self.id
        );
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
        heap().mark_live(mem);

        mem
    }

    #[inline]
    unsafe fn alloc_inside_tlab_fast(&mut self, size: usize) -> *mut u8 {
        self.tlab.allocate(size)
    }

    unsafe fn alloc_inside_tlab_slow(&mut self, size: usize) -> *mut u8 {
        self.tlab.retire(self.id);

        let tlab_size = self.max_tlab_size;
        let mut req = AllocRequest::new(super::AllocType::ForLAB, heap().options().min_tlab_size, tlab_size);
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
        self.safepoint = safepoint::SAFEPOINT_PAGE.address();
        assert_ne!(self.safepoint, null_mut());
        self.stack = StackBounds::current_thread_stack_bounds();
        let sp = approximate_stack_pointer();
        self.last_sp = align_down(sp as _, 8) as _;
        self.mark_ctx = heap().marking_context_mut() as *mut MarkingContext;
        let th = threads();
        th.add_thread(Thread::current());

        for _ in 0..3 {
            self.safepoint();
        }

        let heap = heap();

        self.max_tlab_size = if heap.options().tlab_size > 0 {
            heap.options().tlab_size // TLAB size set by user
        } else {
            heap.options().max_tlab_size // TLAB size set automatically based on heap options
        };

        /*self.queue = vec![null_mut(); heap.options().max_satb_buffer_size].into_boxed_slice();
        self.queue_index = 0;
        self.queue_size = self.queue.len();*/
        let buffer =
            unsafe { libc::malloc(size_of::<usize>() * heap.options().max_satb_buffer_size) };
        self.satb_mark_queue.set_buffer(buffer.cast());
        self.satb_mark_queue
            .set_index(heap.options().max_satb_buffer_size);
        self.biased_begin = heap.card_table().get_biased_begin();
        self.mark_bitmap = heap.marking_context().mark_bitmap();
        self.cm_in_progress = heap.is_concurrent_mark_in_progress(); // Thread migth be attached while marking is running.
    }

    /// SATB write barrier. Ensures that object processes all references correctly.
    /// Must be inserted before write to `handle`.
    #[inline]
    pub fn write_barrier<T: Object + ?Sized>(&mut self, handle: Handle<T>) {
        unsafe {
            self.raw_write_barrier(handle.as_ptr().sub(size_of::<HeapObjectHeader>()).cast());
        }
    }

    pub(crate) fn toggle_write_barrier(&mut self, value: bool) {
        self.cm_in_progress = value;
    }

    /// Raw implementation of write-barrier.
    ///
    /// If SATB mode is used this code will do Yuasa deletion write barrier that captures
    /// writes to white objects. Note that this barrier is very conservative: it does not
    /// check colors of new values that are being written.
    ///
    /// In Incremental Update mode uses Steele's write barrier that captures black<-white writes.
    /// This barrier is very conservative as well. Note that with IU mode concurrent mark termination
    /// might take longer.
    ///
    /// In passive mode does nothing.
    #[inline]
    pub unsafe fn raw_write_barrier(&mut self, obj: *mut HeapObjectHeader) {
        // Yuasa deletion barrier implementation
        // Capture white<-black writes
        #[cfg(feature = "gc-satb")]
        if self.cm_in_progress {
            // Filter marked objects before hitting the SATB queues.
            if !(*self.mark_bitmap).check_bit(obj as _) {
                if !self.satb_mark_queue.try_enqueue(obj as _) {
                    self.slow_write_barrier(obj);
                }
            }
        }

        // Capture black<-white writes
        #[cfg(feature = "gc-incremental-update")]
        if self.cm_in_progress {
            if (*self.mark_bitmap).check_bit(obj as _) {
                // unmark object before putting to queue.
                (*self.mark_bitmap).clear_bit(obj as _);
                if !self.satb_mark_queue.try_enqueue(obj as _) {
                    self.slow_write_barrier(obj);
                }
            }
        }
    }

    #[inline(never)]
    #[cold]
    unsafe fn slow_write_barrier(&mut self, obj: *mut HeapObjectHeader) {
        self.flush_ssb();
        self.raw_write_barrier(obj);
    }

    /// Flush SSB queue. This function is called when SSB queue is full.
    /// 
    /// Writes are pushed to global SATB queue. If object is already marked it is not pushed to SATB queue.
    pub fn flush_ssb(&mut self) {
        let heap = heap();
        unsafe {
            for i in self.satb_mark_queue.index..heap.options().max_satb_buffer_size {
                let obj = self.satb_mark_queue.buf.add(i).read();
                assert!(heap.is_in(obj), "not in heap: {:p} (cm_in_progress?={})", obj, self.cm_in_progress);
                // GC can do some progress in background and already mark object that was in SSB.
                if !(*self.mark_ctx).is_marked(obj as _) || cfg!(feature = "gc-incremental-update")
                {
                    (*self.mark_ctx)
                        .mark_queues()
                        .injector()
                        .push(MarkTask::new(obj as _, false, false));
                }
            }
        }
        self.satb_mark_queue
            .set_index(heap.options().max_satb_buffer_size);
    }

    pub fn atomic_gc_state(&self) -> &AtomicI8 {
        unsafe { std::mem::transmute(&self.gc_state) }
    }

    #[inline]
    pub unsafe fn gc_state_set(&mut self, state: i8, old_state: i8) -> i8 {
        self.atomic_gc_state().store(state, Ordering::Release);
        if old_state != 0 && state == 0 && !self.safepoint.is_null() {
            self.safepoint();
        }

        old_state
    }

    #[inline]
    pub unsafe fn set_last_sp(&mut self, sp: *mut u8) {
        self.last_sp = sp;
    }

    pub unsafe fn state_save_and_set(&mut self, state: i8) -> i8 {
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
        cfg!(feature = "conditional-safepoint")
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

        // Two paths here if conditional safepoints disabled:
        //
        // 1) Safepoint page is not armed, so read does not cause anythin
        // 2) Safepoint page is armed, read causes SIGBUG/SIGSEGV and we go into signal handler
        //    where we would wait for safepoint to be disabled using condvar + mutex.
        //
        let val = unsafe { safepoint.read_volatile() };
        let _ = val;
        #[cfg(feature = "conditional-safepoint")]
        {
            // In case of conditional safepoint armed safepoint value is just set to non-zero.
            // If it is non-zero we go to condvar + mutex wait loop
            if val != 0 {
                self.enter_conditional();
            }
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }

    #[inline(never)]
    #[cold]
    fn enter_conditional(&mut self) {
        #[cfg(not(windows))]
        {
            extern "C" {
                #[allow(improper_ctypes)]
                fn getcontext(ctx: *mut libc::ucontext_t) -> i32;
            }

            unsafe {
                let mut ctx = MaybeUninit::<libc::ucontext_t>::zeroed().assume_init();
                getcontext(&mut ctx as *mut _);

                self.platform_registers = registers_from_ucontext(&mut ctx);
                self.enter_safepoint(approximate_stack_pointer() as _);
                self.platform_registers = null_mut();
                let _ = ctx;
            }
        }
    }

    pub(crate) fn save_registers(&mut self) {
        #[cfg(not(windows))]
        {
            extern "C" {
                #[allow(improper_ctypes)]
                fn getcontext(ctx: *mut libc::ucontext_t) -> i32;
            }

            unsafe {
                let mut ctx = MaybeUninit::<libc::ucontext_t>::zeroed().assume_init();
                getcontext(&mut ctx as *mut _);

                self.platform_registers = registers_from_ucontext(&mut ctx);
                let _ = ctx;
            }
        }
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
    }

    pub(crate) fn set_gc_and_wait(&mut self) {
        let state = self.gc_state;
        self.atomic_gc_state()
            .store(GC_STATE_WAITING, Ordering::Release);
        unsafe {
            super::safepoint::wait_gc();
        }
        self.atomic_gc_state().store(state, Ordering::Release);
    }

    /// Returns true if thread is registered in a GC.
    pub fn is_registered(&self) -> bool {
        !self.safepoint.is_null() && unsafe { self.safepoint != &mut SINK }
    }

    /// Returns current thread.
    pub fn current() -> &'static mut Thread {
        unsafe { &mut THREAD }
    }

    /// Returns registers for current thread. This is used by GC to trace current registers.
    ///
    /// # Safety
    ///
    /// Can return invalid pointer or null. Must be used only by GC and only when roots are traced.
    ///
    /// Should not be used outside of RSGC.
    pub unsafe fn get_registers(&self) -> (*mut u8, usize) {
        (
            self.platform_registers.cast(),
            size_of::<PlatformRegisters>() / size_of::<usize>(),
        )
    }
}

pub struct UnsafeScope {
    state: i8,
    thread: &'static mut Thread,
}

impl UnsafeScope {
    /// Enter unsafe GC state. This means current thread runs "managed by GC code" and GC *must* stop this thread
    /// at GC cycle.
    ///
    /// Returns current state to restore later.
    pub fn new(thread: &'static mut Thread) -> Self {
        Self {
            state: unsafe { thread.state_save_and_set(0) },
            thread,
        }
    }
}

impl Drop for UnsafeScope {
    fn drop(&mut self) {
        unsafe {
            self.thread.gc_state_set(self.state, 0);
        }
    }
}

/// Enters safepoint scope. This means that current thread is in "safe" state and GC can run.
///
/// Note that `cb` MUST not invoke any GC code or access GC objects otherwise UB will happen.
pub fn safepoint_scope_conditional<R>(enter: bool, cb: impl FnOnce() -> R) -> R {
    let thread = Thread::current();
    unsafe {
        extern "C" {
            #[allow(improper_ctypes)]
            fn getcontext(ctx: *mut libc::ucontext_t) -> i32;
        }
        let mut ucontext = MaybeUninit::<libc::ucontext_t>::zeroed().assume_init();

        getcontext(&mut ucontext);
        thread.platform_registers = registers_from_ucontext(&mut ucontext);
        let state = thread.state_save_and_set(if enter { GC_STATE_SAFE } else { 0 });

        thread.set_last_sp(approximate_stack_pointer() as _);
        let cb = AssertUnwindSafe(cb);
        let result = match std::panic::catch_unwind(move || cb()) {
            Ok(result) => result,
            Err(err) => {
                thread.gc_state_set(state, GC_STATE_SAFE);
                thread.platform_registers = null_mut();
                std::panic::resume_unwind(err);
            }
        };

        thread.gc_state_set(state, if enter { GC_STATE_SAFE } else { 0 });
        thread.platform_registers = null_mut();
        result
    }
}

/// Enters safepoint scope. This means that current thread is in "safe" state and GC can run.
///
/// Note that `cb` MUST not invoke any GC code or access GC objects otherwise UB will happen.
pub fn safepoint_scope<R>(cb: impl FnOnce() -> R) -> R {
    safepoint_scope_conditional(true, cb)
}

static mut SINK: u8 = 0;


#[thread_local]
static mut THREAD: Thread = Thread {
    biased_begin: 0,
    id: u64::MAX,
    mark_ctx: null_mut(),
    mark_bitmap: null_mut(),
    tlab: ThreadLocalAllocBuffer::new(),
    stack: StackBounds::none(),
    safepoint: null_mut(),
    last_sp: null_mut(),
    max_tlab_size: 0,
    gc_state: 0,
    satb_mark_queue: LocalSSB::new(),
    cm_in_progress: false,
    platform_registers: null_mut(),
};

use parking_lot::lock_api::RawMutex;
use sync::mutex::RawMutex as Lock;

pub struct Threads {
    pub threads: Mutex<Vec<*mut Thread>>,
    pub cv_join: Condvar,
}

impl Threads {
    pub fn new() -> Self {
        Self {
            threads: Mutex::new(vec![]),
            cv_join: Condvar::new(),
        }
    }

    pub fn add_thread(&self, thread: *mut Thread) {
        let mut threads = self.threads.lock(false);
        threads.push(thread);
    }

    pub fn remove_current_thread(&self) {
        unsafe {
            let thread = Thread::current();
            (*thread).tlab.retire(thread.id);
            (*thread).flush_ssb();
            let raw = thread as *mut Thread;

            safepoint_scope(|| {
                let mut threads = self.threads.lock(true);
                threads.retain(|th| {
                    let th = *th;
                    if th == raw {
                        false
                    } else {
                        true
                    }
                });
            });

            thread.safepoint = &mut SINK;
            self.cv_join.notify_all();
        }
    }

    pub fn join_all(&self) {
        let mut threads = self.threads.lock(true);

        while threads.len() > 0 {
            self.cv_join.wait(&mut threads);
        }
    }

    pub fn get(&self) -> MutexGuard<'_, Vec<*mut Thread>> {
        let threads = self.threads.lock(false);
        threads
    }
}

unsafe impl Sync for Threads {}
unsafe impl Send for Threads {}

static THREADS: once_cell::sync::Lazy<Threads> = once_cell::sync::Lazy::new(Threads::new);

pub(crate) fn threads() -> &'static Threads {
    &THREADS
}

pub struct OOM(pub usize);

/// Marks current thread as 'main' thread. Initializes GC with given arguments and calls `callback`.
///
/// Note that this function does not terminate until all mutator threads are terminated and GC is stopped.
pub fn main_thread<R>(
    args: HeapArguments,
    callback: impl FnOnce(&mut Heap) -> Result<R, Box<dyn std::error::Error>> + UnwindSafe,
) -> Result<R, Box<dyn std::error::Error>> {
    let heap = Heap::new(args);
    Thread::current().register();
    let res = std::panic::catch_unwind(|| callback(super::heap::heap()));

    safepoint_scope(|| {
        threads().remove_current_thread();
        threads().join_all();
    });

    unsafe {
        heap.stop();
    }

    match res {
        Ok(res) => res,
        Err(err) => {
            std::panic::resume_unwind(err);
        }
    }
}

/// Spawns new thread and registers it with GC.
pub fn spawn_thread<F, R>(cb: F) -> GCAwareJoinHandle<R>
where
    F: 'static + FnOnce() -> R + Send + UnwindSafe,
    R: 'static + Send,
{
    let join = safepoint_scope(|| {
        let join = std::thread::spawn(move || {
            Thread::current().register();
            let res = std::panic::catch_unwind(|| cb());

            threads().remove_current_thread();

            match res {
                Ok(val) => val,
                Err(err) => std::panic::resume_unwind(err),
            }
        });
        join
    });
    GCAwareJoinHandle { join }
}


pub struct GCAwareJoinHandle<R> {
    join: JoinHandle<R>,
}

impl<R> GCAwareJoinHandle<R> {
    pub fn join(self) -> Result<R, Box<dyn Any + Send>> {
        let res = safepoint_scope(move || {
            let res = self.join.join();
            res
        });

        res
    }
}

pub mod scoped {
    //! Scoped mutator threads.
    use std::{
        marker::PhantomData,
        panic::{AssertUnwindSafe, UnwindSafe},
        sync::{
            atomic::{AtomicBool, AtomicUsize},
            Arc,
        },
        thread::Thread,
    };

    use atomic::Ordering;

    use crate::heap::stack::approximate_stack_pointer;

    use super::GC_STATE_SAFE;

    pub struct Scope<'a, 'b> {
        scope: &'a std::thread::Scope<'a, 'b>,
    }

    impl<'a, 'b> Scope<'a, 'b> {
        pub fn spawn<F>(&mut self, cb: F)
        where
            F: FnOnce() + Send + 'b,
        {
            self.scope.spawn(move || {
                super::Thread::current().register();
                let wrapper = AssertUnwindSafe(cb);
                let res = std::panic::catch_unwind(move || {
                    wrapper();
                });

                super::threads().remove_current_thread();

                match res {
                    Ok(val) => val,
                    Err(err) => std::panic::resume_unwind(err),
                }
            });
        }
    }

    /// GC-aware [std::thread::scope].
    pub fn scoped(cb: impl FnOnce(&mut Scope)) {
        let thread = super::Thread::current();
        let mut state = 0;
        std::thread::scope(|scope| unsafe {
            let mut scope = Scope { scope };

            cb(&mut scope);

            // enter safepoint here since scoepd threads might not be finished there
            // and when `scope` is dropped, the threads are joined and our thread waits
            // for them to join
            thread.last_sp = approximate_stack_pointer() as _;
            state = thread.state_save_and_set(GC_STATE_SAFE);
        });
        unsafe {
            thread.gc_state_set(state, GC_STATE_SAFE);
        }
    }
}
