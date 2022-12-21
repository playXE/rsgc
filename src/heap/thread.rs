use std::{
    any::Any,
    cell::UnsafeCell,
    collections::HashMap,
    intrinsics::{likely, unlikely},
    mem::{size_of, MaybeUninit},
    panic::UnwindSafe,
    ptr::{null, null_mut},
    sync::atomic::{AtomicI8, AtomicUsize, Ordering},
    thread::{JoinHandle, ThreadId},
};

use crate::{
    formatted_size,
    heap::{mark::MarkTask, stack::approximate_stack_pointer, align_down},
    object::{Allocation, ConstVal, Handle, HeapObjectHeader, SizeTag, VTable, VtableTag, VT},
    sync::{
        self,
        mutex::{Condvar, Mutex, MutexGuard},
    },
    traits::Object,
    utils::{deque::LocalSSB, machine_context::{PlatformRegisters, registers_from_ucontext}},
};
use crate::{heap::tlab::ThreadLocalAllocBuffer, utils::ptr_queue::PtrQueueImpl};

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
    pub(crate) id: ThreadId,
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

impl Thread {
    pub unsafe fn satb_mark_queue(&self) -> &LocalSSB {
        &self.satb_mark_queue
    }

    pub unsafe fn satb_mark_queue_mut(&mut self) -> &mut LocalSSB {
        &mut self.satb_mark_queue
    }

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
            //(*obj).clear_marked();
            //(*obj).set_finalization_ordering(false);
            if T::NO_HEAP_PTRS {
                (*obj).set_no_heap_ptrs();
            }
            obj.add(1).cast::<T>().write(value);
            
            // We implement black mutator technique and SATB for concurrent marking.
            //
            // This means we collect garbage that was allocated *before* the cycle started
            // and thus all new objects should implicitly be marked as black. This has problem
            // of floating garbage but it significantly simplifies write barrier implementation.
            (*self.mark_bitmap).set_bit(obj as _);
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
            if T::NO_HEAP_PTRS {
                (*obj).set_no_heap_ptrs();
            }
            obj.add(1)
                .cast::<u8>()
                .add(T::VARSIZE_OFFSETOF_LENGTH)
                .cast::<usize>()
                .write(length);

            //debug_assert!((*self.mark_ctx).is_marked(obj as _));
            (*self.mark_bitmap).atomic_test_and_set(obj as _);
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
        let mut req = AllocRequest::new(super::AllocType::ForLAB, 2 * 1024, tlab_size);
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
    /// Must be inserted after write to `handle`.
    #[inline]
    pub fn write_barrier<T: Object + ?Sized>(&mut self, handle: Handle<T>) {
        unsafe {
            self.raw_write_barrier(handle.as_ptr().sub(size_of::<HeapObjectHeader>()).cast());
        }
    }

    pub(crate) fn toggle_write_barrier(&mut self, value: bool) {
        self.cm_in_progress = value;
    }

    /// Yuasa deletion barrier implementation
    #[inline]
    pub unsafe fn raw_write_barrier(&mut self, obj: *mut HeapObjectHeader) {
        if self.cm_in_progress {
            // Filter marked objects before hitting the SATB queues.
            if !(*self.mark_bitmap).check_bit(obj as _) {
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

    pub(crate) unsafe fn flush_ssb(&mut self) {
        let heap = heap();
        for i in self.satb_mark_queue.index..heap.options().max_satb_buffer_size {
            let obj = self.satb_mark_queue.buf.add(i).read();
            // GC can do some progress in background and already mark object that was in SSB.
            if !(*self.mark_ctx).is_marked(obj as _) {
                (*self.mark_ctx)
                    .mark_queues()
                    .injector()
                    .push(MarkTask::new(obj as _, false, false));
            }
        }

        self.satb_mark_queue
            .set_index(heap.options().max_satb_buffer_size);
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
        #[cfg(not(windows))] {
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
                drop(ctx);
            }
        }
       
    }

    pub(crate) fn save_registers(&mut self) {
        #[cfg(not(windows))] {
            extern "C" {
                #[allow(improper_ctypes)]
                fn getcontext(ctx: *mut libc::ucontext_t) -> i32;
            }

            unsafe {
                let mut ctx = MaybeUninit::<libc::ucontext_t>::zeroed().assume_init();
                getcontext(&mut ctx as *mut _);

                self.platform_registers = registers_from_ucontext(&mut ctx);
                drop(ctx);
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
        super::safepoint::wait_gc();
        self.atomic_gc_state().store(state, Ordering::Release);
    }

    /// Returns true if thread is registered in a GC.
    pub fn is_registered(&self) -> bool {
        !self.safepoint.is_null() && unsafe { self.safepoint != &mut SINK }
    }

    pub fn current() -> &'static mut Thread {
        THREAD.with(|thread| unsafe { &mut *thread.get() })
    }

    pub fn get_registers(&self) -> (*mut u8, usize) {
        (self.platform_registers.cast(), size_of::<PlatformRegisters>())
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
    #[cfg(not(windows))]
    ucontext: Box<libc::ucontext_t>,
    thread: &'a mut Thread,
}

impl<'a> SafeScope<'a> {
    /// Enter safe GC state. This means current thread runs "unmanaged by GC code" and GC does not need to stop this thread
    /// at GC cycle.
    ///
    /// Returns current state to restore later.
    pub fn new(thread: &'a mut Thread) -> Self {
        thread.set_last_sp(approximate_stack_pointer() as _);
        let mut this = Self {
            state: thread.state_save_and_set(GC_STATE_SAFE),
            thread,
            #[cfg(not(windows))]
            ucontext: Box::new(unsafe {
                MaybeUninit::zeroed().assume_init()
            })
        };

        #[cfg(not(windows))] {
            extern "C" {
                #[allow(improper_ctypes)]
                fn getcontext(ctx: *mut libc::ucontext_t) -> i32;
            }

            unsafe {
                getcontext(this.ucontext.as_mut());
                this.thread.platform_registers = registers_from_ucontext(this.ucontext.as_mut());
            }
        }

        this
    }
}

impl<'a> Drop for SafeScope<'a> {
    fn drop(&mut self) {
        self.thread.gc_state_set(self.state, GC_STATE_SAFE);
        self.thread.platform_registers = null_mut();
    }
}

static mut SINK: u8 = 0;

thread_local! {
    static THREAD: UnsafeCell<Thread> = UnsafeCell::new(
        Thread {
            biased_begin: 0,
            id: std::thread::current().id(),
            mark_ctx: null_mut(),
            mark_bitmap: null_mut(),
            tlab: ThreadLocalAllocBuffer::new(),
            stack: StackBounds::current_thread_stack_bounds(),
            safepoint: unsafe { &mut SINK },
            last_sp: null_mut(),
            max_tlab_size: 0,
            gc_state: 0,
            satb_mark_queue: LocalSSB::new(),
            cm_in_progress: false,
            platform_registers: null_mut()
        });
}

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
            (*thread).tlab.retire(std::thread::current().id());
            (*thread).flush_ssb();
            let raw = thread as *mut Thread;
            let safe_scope = SafeScope::new(thread);

            let mut threads = self.threads.lock(true);
            threads.retain(|th| {
                let th = *th;
                if th == raw {
                    false
                } else {
                    true
                }
            });

            drop(safe_scope);
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

pub fn main_thread(args: HeapArguments, callback: impl FnOnce(&mut Heap) + UnwindSafe) {
    let heap = Heap::new(args);
    Thread::current().register();
    let res = std::panic::catch_unwind(|| callback(super::heap::heap()));

    let scope = SafeScope::new(Thread::current());
    threads().remove_current_thread();
    threads().join_all();
    drop(scope);

    unsafe {
        heap.stop();
    }

    match res {
        Ok(_) => (),
        Err(err) => std::panic::resume_unwind(err),
    }
}

pub fn spawn_thread<F, R>(cb: F) -> GCAwareJoinHandle<R>
where
    F: 'static + FnOnce() -> R + Send + UnwindSafe,
    R: 'static + Send,
{
    let thread = Thread::current();
    let scope = SafeScope::new(thread);
    let join = std::thread::spawn(move || {
        Thread::current().register();
        let res = std::panic::catch_unwind(|| cb());

        threads().remove_current_thread();

        match res {
            Ok(val) => val,
            Err(err) => std::panic::resume_unwind(err),
        }
    });
    drop(scope);
    GCAwareJoinHandle { join }
}

pub struct GCAwareJoinHandle<R> {
    join: JoinHandle<R>,
}

impl<R> GCAwareJoinHandle<R> {
    pub fn join(self) -> Result<R, Box<dyn Any + Send>> {
        let scope = SafeScope::new(Thread::current());
        let res = self.join.join();
        drop(scope);

        res
    }
}

pub mod scoped {
    use std::{
        marker::PhantomData,
        panic::{UnwindSafe, AssertUnwindSafe},
        sync::{
            atomic::{AtomicBool, AtomicUsize},
            Arc,
        },
        thread::Thread,
    };

    use atomic::Ordering;

    use super::SafeScope;

    pub struct Scope<'a, 'b> {
        scope: &'a std::thread::Scope<'a, 'b>,
    }

    impl<'a, 'b> Scope<'a, 'b> {
        pub fn spawn<F>(&mut self, cb: F)
        where F: FnOnce() + Send + 'b,
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

    pub fn scoped(cb: impl FnOnce(&mut Scope)) {
        let scope = std::thread::scope(|scope| {
            let mut scope = Scope { scope };

            cb(&mut scope);
            let safe_scope = SafeScope::new(super::Thread::current());
            safe_scope
        });
        drop(scope);

    }
}
