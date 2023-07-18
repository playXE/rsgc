
#![allow(dead_code)]
#[cfg(not(windows))]
use libc::*;
#[cfg(windows)]
use winapi::um::{memoryapi::*, winnt::*};

#[cfg(not(windows))]
const HEAP_MEM_PROT: c_int = PROT_READ | PROT_WRITE;
#[cfg(not(windows))]
const HEAP_MEM_FLAGS: c_int = MAP_NORESERVE | MAP_PRIVATE | MAP_ANONYMOUS;
#[cfg(all(not(linux), not(windows)))]
const MAP_POPULATE: c_int = 0;
#[cfg(not(windows))]
const HEAP_MEM_FLAGS_PREALLOC: c_int = MAP_NORESERVE | MAP_PRIVATE | MAP_ANONYMOUS | MAP_POPULATE;

pub unsafe fn memory_map(memory_size: usize) -> *mut u8 {
    #[cfg(windows)]
    {
        // On Windows only reserve given chunk of memory. It should be explicitly
        // committed later.
        // We don't use MAP_PHYSICAL flag to prevent usage of swap file, since it
        // supports only 32-bit address space and is in most cases not recommended.
        VirtualAlloc(
            core::ptr::null_mut(),
            memory_size,
            MEM_RESERVE,
            PAGE_NOACCESS,
        ) as _
    }

    #[cfg(not(windows))]
    {
        mmap(
            core::ptr::null_mut(),
            memory_size,
            HEAP_MEM_PROT,
            HEAP_MEM_FLAGS,
            -1,
            0,
        ) as _
    }
}

pub unsafe fn memory_unmap(ptr: *mut u8, memory_size: usize) {
    #[cfg(windows)]
    {
        VirtualFree(ptr as _, 0, MEM_RELEASE);
    }

    #[cfg(not(windows))]
    {
        munmap(ptr as _, memory_size);
    }
}

pub unsafe fn memory_map_prealloc(memory_size: usize, do_prealloc: bool) -> *mut u8 {
    #[cfg(windows)]
    {
        let _ = do_prealloc;
        return memory_map(memory_size);
    }

    #[cfg(not(windows))]
    {
        if !do_prealloc {
            return memory_map(memory_size);
        }

        let ptr = mmap(
            core::ptr::null_mut(),
            memory_size,
            HEAP_MEM_PROT,
            HEAP_MEM_FLAGS_PREALLOC,
            -1,
            0,
        ) as _;

        #[cfg(not(linux))]
        {
            // if we are not on linux the next best thing we can do is to mark the pages
            // as MADV_WILLNEED but only if do_prealloc is enabled.
            madvise(ptr as _, memory_size, MADV_WILLNEED);
        }

        ptr
    }
}

pub unsafe fn memory_commit(ptr: *mut u8, memory_size: usize) -> bool {
    #[cfg(windows)]
    {
        VirtualAlloc(ptr as _, memory_size, MEM_COMMIT, PAGE_READWRITE) as usize != 0
    }

    #[cfg(not(windows))]
    {
        let _ = ptr;
        let _ = memory_size;
        //no need for commit on linux
        true
    }
}