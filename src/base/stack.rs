use std::{
    mem::size_of,
    ptr::{null, DynMetadata},
};

extern "C" {
    #[link_name = "llvm.frameaddress"]
    fn builtin_frame_address(level: i32) -> *const u8;
}

pub fn get_current_stack_position() -> *const u8 {
    unsafe { builtin_frame_address(0) }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Stack {
    stack_start: *const u8,
}

impl Stack {
    pub fn stack_start(&self) -> *const u8 {
        self.stack_start
    }
    pub fn new() -> Self {
        Self {
            stack_start: StackBounds::current_thread_stack_bounds().origin as _,
        }
    }
    pub fn is_on_stack(&self, addr: *const u8) -> bool {
        get_current_stack_position() <= addr && addr <= self.stack_start
    }
    #[inline(never)]
    pub fn iterate_pointers(&self, visitor: &mut dyn StackVisitor) {
        let ptr = visitor as *mut dyn StackVisitor;
        let mut raw = ptr.to_raw_parts();

        iterate_pointers_impl(
            self,
            &mut raw as *mut (_, _) as _,
            approximate_stack_pointer() as _,
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct StackBounds {
    pub origin: *mut u8,
    pub bound: *mut u8,
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
impl StackBounds {
    pub unsafe fn new_thread_stack_bounds(thread: libc::pthread_t) -> Self {
        let origin = libc::pthread_get_stackaddr_np(thread);
        let size = libc::pthread_get_stacksize_np(thread);
        let bound = origin.add(size);
        Self {
            origin: origin.cast(),
            bound: bound.cast(),
        }
    }
    pub fn current_thread_stack_bounds() -> Self {
        unsafe { Self::new_thread_stack_bounds(thread_self() as _) }
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
impl StackBounds {
    #[cfg(target_os = "openbsd")]
    unsafe fn new_thread_stack_bounds(thread: libc::pthread_t) -> Self {
        let mut stack: libc::stack_t = core::mem::MaybeUninit::zeroed().assume_init();
        libc::pthread_stackseg_np(thread, &mut stack);
        let origin = stack.ss_sp;
        let bound = stack.origin.sub(stack.ss_size);
        return Self {
            origin: origin.cast(),
            bound: bound.cast(),
        };
    }

    #[cfg(not(target_os = "openbsd"))]
    unsafe fn new_thread_stack_bounds(thread: libc::pthread_t) -> Self {
        let mut bound = core::ptr::null_mut::<libc::c_void>();
        let mut stack_size = 0;
        let mut sattr: libc::pthread_attr_t = core::mem::MaybeUninit::zeroed().assume_init();
        libc::pthread_attr_init(&mut sattr);
        #[cfg(any(target_os = "freebsd", target_os = "netbsd"))]
        {
            libc::pthread_attr_get_np(thread, &mut sattr);
        }
        #[cfg(not(any(target_os = "freebsd", target_os = "netbsd")))]
        {
            libc::pthread_getattr_np(thread, &mut sattr);
        }
        let _rc = libc::pthread_attr_getstack(&sattr, &mut bound, &mut stack_size);
        libc::pthread_attr_destroy(&mut sattr);
        let origin = bound.add(stack_size);
        Self {
            bound: bound.cast(),
            origin: origin.cast(),
        }
    }

    pub fn current_thread_stack_bounds() -> Self {
        unsafe { Self::new_thread_stack_bounds(crate::thread_self() as _) }
    }
}

#[cfg(windows)]
impl StackBounds {
    pub unsafe fn current_thread_stack_bounds_internal() -> Self {
        use winapi::um::memoryapi::*;
        use winapi::um::winnt::*;
        let mut stack_origin: MEMORY_BASIC_INFORMATION =
            core::mem::MaybeUninit::zeroed().assume_init();
        VirtualQuery(
            &mut stack_origin as *mut MEMORY_BASIC_INFORMATION as *mut _,
            &mut stack_origin,
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        );

        let origin = stack_origin
            .BaseAddress
            .cast::<u8>()
            .add(stack_origin.RegionSize as _);
        // The stack on Windows consists out of three parts (uncommitted memory, a guard page and present
        // committed memory). The 3 regions have different BaseAddresses but all have the same AllocationBase
        // since they are all from the same VirtualAlloc. The 3 regions are laid out in memory (from high to
        // low) as follows:
        //
        //    High |-------------------|  -----
        //         | committedMemory   |    ^
        //         |-------------------|    |
        //         | guardPage         | reserved memory for the stack
        //         |-------------------|    |
        //         | uncommittedMemory |    v
        //    Low  |-------------------|  ----- <--- stackOrigin.AllocationBase
        //
        // See http://msdn.microsoft.com/en-us/library/ms686774%28VS.85%29.aspx for more information.
        let mut uncommitted_memory: MEMORY_BASIC_INFORMATION =
            core::mem::MaybeUninit::zeroed().assume_init();
        VirtualQuery(
            stack_origin.AllocationBase as *mut _,
            &mut uncommitted_memory,
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        );
        let mut guard_page: MEMORY_BASIC_INFORMATION =
            core::mem::MaybeUninit::zeroed().assume_init();
        VirtualQuery(
            uncommitted_memory
                .BaseAddress
                .cast::<u8>()
                .add(uncommitted_memory.RegionSize as _)
                .cast(),
            &mut guard_page,
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        );
        let end_of_stack = stack_origin.AllocationBase as *mut u8;
        let bound = end_of_stack.add(guard_page.RegionSize as _);
        Self {
            origin: origin as *mut u8,
            bound,
        }
    }

    pub fn current_thread_stack_bounds() -> Self {
        unsafe { Self::current_thread_stack_bounds_internal() }
    }
}

fn thread_self() -> u64 {
    #[cfg(unix)]
    unsafe {
        libc::pthread_self() as _
    }

    #[cfg(windows)]
    unsafe {
        extern "C" {
            fn GetCurrentThreadId() -> u32;
        }
        GetCurrentThreadId() as u64
    }
}

extern "C" {
    pub fn PushAllRegistersAndIterateStack(
        stack: &Stack,
        vis: *mut u8,
        callback: extern "C" fn(&Stack, *mut u8, *mut usize),
    );
}

#[inline(never)]
pub fn approximate_stack_pointer() -> *const *const u8 {
    let mut x: *const *const u8 = null();
    x = &x as *const *const *const u8 as *const *const u8;
    x
}

pub trait StackVisitor {
    fn visit_pointer(&mut self, origin: *const *const u8, address: *const u8);
}

impl<F> StackVisitor for F
where
    F: FnMut(*const *const u8, *const u8),
{
    fn visit_pointer(&mut self, origin: *const *const u8, address: *const u8) {
        (self)(origin, address);
    }
}

#[inline(never)]
extern "C" fn iterate_pointers_impl(stack: &Stack, visitor: *mut u8, stack_end: *mut usize) {
    unsafe {
        let tuple = visitor.cast::<(*mut u8, DynMetadata<dyn StackVisitor>)>();
        let tuple = tuple.read();
        let data = tuple.0;
        let meta = tuple.1;
        let visitor = std::ptr::from_raw_parts_mut::<dyn StackVisitor>(data as *mut (), meta);

        let min_stack_alignment = size_of::<usize>();
        let mut current = stack_end as *mut *mut u8;
        assert_eq!(0, current as usize & (min_stack_alignment - 1));

        while current < stack.stack_start() as *mut *mut u8 {
            let address = current.read();
            if address.is_null() {
                current = current.add(1);
                continue;
            }

            (*visitor).visit_pointer(current as *const _, address);
            current = current.add(1);
        }
    }
}
