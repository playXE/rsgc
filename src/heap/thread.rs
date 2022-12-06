use std::{
    cell::UnsafeCell,
    collections::HashMap,
    intrinsics::unlikely,
    ptr::null_mut,
    sync::atomic::{AtomicI8, Ordering},
};

use parking_lot::{Mutex, MutexGuard};

use crate::heap::stack::approximate_stack_pointer;

use super::{safepoint, stack::StackBounds};

// gc_state = 1 means the thread is doing GC or is waiting for the GC to
//              finish.
pub const GC_STATE_WAITING: i8 = 1;
// gc_state = 2 means the thread is running unmanaged code that can be
//              execute at the same time with the GC.
pub const GC_STATE_SAFE: i8 = 2;

pub struct ThreadInfo {
    stack: StackBounds,
    safepoint: *mut u8,
    last_sp: *mut u8,
    gc_state: i8,
    safepoint_tasks: HashMap<usize, Box<dyn FnMut()>>,
    free_keys: Vec<usize>,
}

impl ThreadInfo {
    pub fn atomic_gc_state(&self) -> &AtomicI8 {
        unsafe { std::mem::transmute(&self.gc_state) }
    }

    

    #[inline]
    pub(crate) fn gc_state_set(&mut self, state: i8, old_state: i8) -> i8 {
        self.atomic_gc_state().store(state, Ordering::Release);
        if old_state != 0 && state == 0 {
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


    /// Reads from polling page. If safepoint is disabled nothing happens
    /// but when safepoint is enabled this triggers page fault (SIGSEGV/SIGBUS on Linux/macOS/BSD)
    /// and goes into signal to suspend thread. 
    #[inline(always)]
    pub fn safepoint(&mut self) {
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
        let safepoint = self.safepoint;
        let val = unsafe {
            safepoint.read_volatile();
        };
        let _ = val;
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
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

    #[cold]
    pub(crate) fn register(&mut self) {
        self.safepoint = safepoint::SAFEPOINT_PAGE.load(Ordering::Relaxed);
        assert_ne!(self.safepoint, null_mut());
        self.stack = StackBounds::current_thread_stack_bounds();
        self.last_sp = approximate_stack_pointer() as _;
        acquire_threads().push(self as *mut Self);

        for _ in 0..3 {
            self.safepoint();
        }
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
}

/// Get [ThreadInfo] reference for allocating memory and accessing GC APIs.
///
/// If thread is not registered by a GC it is first registered and then reference is returned.
pub fn thread() -> &'static mut ThreadInfo {
    unsafe {
        let thread = THREAD.with(|th| th.get());
        if unlikely((*thread).safepoint.is_null()) {
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

pub struct SafeScope {
    state: i8,
    thread: &'static mut ThreadInfo,
}

impl SafeScope {
    /// Enter safe GC state. This means current thread runs "unmanaged by GC code" and GC does not need to stop this thread
    /// at GC cycle.
    ///
    /// Returns current state to restore later.
    pub fn new(thread: &'static mut ThreadInfo) -> Self {
        thread.set_last_sp(approximate_stack_pointer() as _);
        Self {
            state: thread.state_save_and_set(GC_STATE_SAFE),
            thread,
        }
    }
}

impl Drop for SafeScope {
    fn drop(&mut self) {
        self.thread.gc_state_set(self.state, GC_STATE_SAFE);
    }
}

thread_local! {
    static THREAD: UnsafeCell<ThreadInfo> = UnsafeCell::new(
        ThreadInfo {
            stack: StackBounds::current_thread_stack_bounds(),
            safepoint: null_mut(),
            last_sp: null_mut(),
            gc_state: 0,
            safepoint_tasks: HashMap::new(),
            free_keys: vec![],
        });
}

impl Drop for ThreadInfo {
    fn drop(&mut self) {
        let current = self as *mut Self;
        
        acquire_threads().retain(|thread| {
            let thread = *thread;

            thread != current
        });
    }
}

pub struct Threads {
    pub threads: Mutex<Vec<*mut ThreadInfo>>,
}

unsafe impl Sync for Threads {}
unsafe impl Send for Threads {}

static THREADS: once_cell::sync::Lazy<Threads> = once_cell::sync::Lazy::new(|| Threads {
    threads: Mutex::new(vec![]),
});

pub(crate) fn acquire_threads<'a>() -> MutexGuard<'a, Vec<*mut ThreadInfo>> {
    THREADS.threads.lock()
}

pub(crate) unsafe fn wait_for_the_world<'a>() -> MutexGuard<'a, Vec<*mut ThreadInfo>> {
    let threads = acquire_threads();
    for i in 0..threads.len() {
        let th = &*threads[i];

        // This acquire load pairs with the release stores
        // in the signal handler of safepoint so we are sure that
        // all the stores on those threads are visible.
        // We're currently also using atomic store release in mutator threads
        // (in gc_state_set), but we may want to use signals to flush the
        // memory operations on those threads lazily instead.
        while th.atomic_gc_state().load(Ordering::Relaxed) == 0
            || th.atomic_gc_state().load(Ordering::Acquire) == 0
        {
            std::hint::spin_loop();
        }
    }

    threads
}
