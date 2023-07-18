use crate::{
    allocator::Allocator, stack::{StackBounds, approximate_stack_pointer}, utils::{mcontext::PlatformRegisters, math::{round_to_next_multiple, div_and_round_up, log2ceil}}, ThreadState, heap::Heap, block_allocator::BlockAllocator, traits::{Object, Allocation}, object_header::{Handle, ObjectHeader, RTTIOf, ConstVal}, constants::{ALLOCATION_ALIGNMENT, BLOCK_TOTAL_SIZE, LARGE_BLOCK_SIZE}, meta::object::ObjectMeta, safepoint::SAFEPOINT_PAGE,
};
use parking_lot::lock_api::RawMutex;
use std::{
    mem::{MaybeUninit, size_of},
    ptr::{null_mut, write_bytes},
    sync::atomic::{AtomicBool, AtomicPtr, Ordering}, intrinsics::unlikely,
};

#[allow(dead_code)]
pub struct Thread {
   
    pub(crate) allocator: MaybeUninit<Allocator>,
    pub(crate) state: ThreadState,
    pub(crate) stack_top: AtomicPtr<*const u8>,
    pub(crate) is_waiting: AtomicBool,
    pub(crate) execution_context: MaybeUninit<PlatformRegisters>,
    pub(crate) safepoint_addr: *const u8,
    pub(crate) initialized: bool,
    pub(crate) stack: StackBounds,
    pub(crate) heap: *mut Heap,
}
#[allow(dead_code)]
impl Thread {
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
        self.set_gc_and_wait();
    }

    pub(crate) fn set_gc_and_wait(&mut self) {
        self.is_waiting.store(true, Ordering::Release);
        self.state = ThreadState::Unmanaged;

        super::safepoint::wait();

        self.is_waiting.store(false, Ordering::Release);
        self.state = ThreadState::Managed;
    }
    #[inline]
    pub fn safepoint(&self) {
        unsafe {
            let _ = self.safepoint_addr.read_volatile();
        }
    }

    pub fn current() -> &'static mut Thread {
        unsafe { &mut THREAD }
    }
    #[inline]
    pub fn allocate<T: Object + Allocation>(&mut self, val: T) -> Handle<T> {
        unsafe {
            let size = T::SIZE + size_of::<ObjectHeader>();
            let size = round_to_next_multiple(size, ALLOCATION_ALIGNMENT);
            let header = if unlikely(size >= LARGE_BLOCK_SIZE) {
                self.alloc_large(size)
            } else {
                self.alloc_small(size)
            }.cast::<ObjectHeader>();
            
            header.write(ObjectHeader { rtti: RTTIOf::<T>::VAL });
            let ptr = header.add(1).cast::<T>();
            ptr.write(val);
            std::mem::transmute(ptr)
        }
    }
    #[inline]
    unsafe fn alloc_small(&mut self, size: usize) -> *mut usize {
        let allocator = self.allocator.assume_init_mut();

        let start = allocator.cursor;
        let end = start.cast::<u8>().add(size).cast::<usize>();
        if end >= allocator.limit {
            return self.alloc_small_slow(size);
        }

        allocator.cursor = end;

        write_bytes(start.cast::<u8>(), 0, size);

        let object = start.cast::<ObjectHeader>();
        let object_meta = (*allocator.bytemap).get(object.cast());
        object_meta.write(ObjectMeta::Allocated);
        
        object.cast()
    }

    #[inline(never)]
    unsafe fn alloc_small_slow(&mut self, size: usize) -> *mut usize {
        let allocator = self.allocator.assume_init_mut();

        let mut object = allocator.alloc(size);

        loop {
            if !object.is_null() {
                break;
            }
            (*self.heap).collect();
            object = allocator.alloc(size);

            if !object.is_null() {
                break;
            }

            (*self.heap).grow(1);

            object = allocator.alloc(size);

            if !object.is_null() {
                break;
            }

            if !(*self.heap).is_growing_possible(1) {
                eprintln!("Out of memory");
                std::process::abort();
            }
        }

        let object_meta = (*allocator.bytemap).get(object.cast());
        object_meta.write(ObjectMeta::Allocated);

        object.cast()
    }

    unsafe fn alloc_large(&mut self, size: usize) -> *mut usize {
        let mut object = (*self.heap).large_allocator.get_block(size);

        if !object.is_null() {
            return object.cast();
        } else {
            let mut pow2increment = 0;

            loop {
                (*self.heap).collect();

                object = (*self.heap).large_allocator.get_block(size);

                if !object.is_null() {
                    return object.cast();
                } else {
                    if pow2increment == 0 {
                        let increment = div_and_round_up(size, BLOCK_TOTAL_SIZE);
                        pow2increment = 1 << log2ceil(increment);
                    }

                    (*self.heap).grow(pow2increment);

                    object = (*self.heap).large_allocator.get_block(size);

                    if !object.is_null() {
                        return object.cast();
                    }
                }

                if !(*self.heap).is_growing_possible(pow2increment) {
                    break;
                }
            }
        }

        null_mut()
    }
}

#[thread_local]
static mut THREAD: Thread = Thread {
    allocator: MaybeUninit::uninit(),
    state: ThreadState::Unmanaged,
    stack_top: AtomicPtr::new(null_mut()),
    is_waiting: AtomicBool::new(false),
    execution_context: MaybeUninit::uninit(),
    safepoint_addr: null_mut(),
    heap: null_mut(),
    stack: StackBounds {
        origin: null_mut(),
        bound: null_mut(),
    },
    initialized: false,
}; 

pub(crate) struct ThreadNode {
    pub thread: &'static mut Thread,
    pub next: *mut ThreadNode
}

pub fn manage_thread() {
    unsafe {
        let thread = &mut THREAD;

        if thread.initialized {
            panic!("Thread is already initialized")
        }
        let heap = Heap::get();
        thread.stack = StackBounds::current_thread_stack_bounds();
        let block_alloc = &mut heap.block_allocator as *mut BlockAllocator;
        thread.allocator = MaybeUninit::new(Allocator::new(heap.block_meta_start, heap.bytemap, block_alloc, heap.heap_start));
        thread.stack_top.store(approximate_stack_pointer() as _, Ordering::Relaxed);
        thread.state = ThreadState::Managed;
        thread.heap = Heap::get();
        heap.thread_mod_lock.lock();
        heap.threads = Box::into_raw(Box::new(ThreadNode {
            thread: &mut THREAD,
            next: heap.threads,
        }));
        thread.safepoint_addr = SAFEPOINT_PAGE.load(Ordering::Relaxed);

        heap.thread_mod_lock.unlock();

        thread.initialized = true;

        thread.allocator.assume_init_mut().init_cursors();
    }
}   

pub fn unmanage_thread() {
    unsafe {
        let thread = &mut THREAD;
        if !thread.initialized {
            panic!("Thread is not initialized")
        }
        let heap = Heap::get();
        heap.thread_mod_lock.lock();
        let mut prev = null_mut::<ThreadNode>();
        let mut current = heap.threads;
        while !current.is_null() {
            if (*current).thread as *mut Thread == thread {
                if prev.is_null() {
                    heap.threads = (*current).next;
                } else {
                    (*prev).next = (*current).next;
                }
                break;
            }
            prev = current;
            current = (*current).next;
        }

        let _ = Box::from_raw(current);
        heap.thread_mod_lock.unlock();
        thread.initialized = false;
    }
}