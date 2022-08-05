use super::{memory_region::MemoryRegion, utils::round_down};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Protection {
    NoAccess,
    ReadOnly,
    ReadWrite,
    ReadExecute,
    ReadWriteExecute,
}

pub struct VirtualMemory {
    region: MemoryRegion,
    alias: MemoryRegion,
    reserved: MemoryRegion,
}

impl VirtualMemory {
    pub fn new(region: MemoryRegion, alias: MemoryRegion, reserved: MemoryRegion) -> Self {
        Self {
            region,
            alias,
            reserved,
        }
    }
    pub fn start(&self) -> usize {
        self.region.start()
    }

    pub fn end(&self) -> usize {
        self.region.end()
    }

    pub fn address(&self) -> *mut u8 {
        self.region.pointer()
    }

    pub fn size(&self) -> usize {
        self.region.size()
    }

    pub fn alias_offset(&self) -> usize {
        self.alias.start() - self.region.start()
    }

    pub fn contains(&self, address: usize) -> bool {
        self.region.contains(address)
    }

    pub fn contains_alias(&self, address: usize) -> bool {
        self.alias_offset() != 0 && self.alias.contains(address)
    }

    pub fn vm_owns_region(&self) -> bool {
        !self.reserved.pointer().is_null()
    }

    pub fn protect(&self, mode: Protection) {
        unsafe { Self::raw_protect(self.address(), self.size(), mode) }
    }

    pub fn in_same_page(address0: usize, address1: usize) -> bool {
        round_down(address0 as _, page_size() as _) == round_down(address1 as _, page_size() as _)
    }

    pub fn truncate(&mut self, new_size: usize) {
        if self.reserved.size() == self.region.size() {
            unsafe {
                if Self::free_sub_segment((self.start() + new_size) as _, self.size() - new_size) {
                    self.reserved.set_size(new_size);

                    if self.alias_offset() != 0 {
                        Self::free_sub_segment(
                            (self.alias.start() + new_size) as _,
                            self.alias.size() - new_size,
                        );
                    }
                }
            }
        }
        let region = self.region;
        self.region.subregion(&region, 0, new_size);
        let alias = self.alias;
        self.alias.subregion(&alias, 0, new_size);
    }
}

#[cfg(unix)]
impl VirtualMemory {}

#[cfg(unix)]
mod vm {
    use std::ptr::null_mut;

    use crate::base::{
        memory_region::MemoryRegion,
        utils::{round_down, round_up},
    };

    use super::{page_size, Protection, VirtualMemory};

    pub unsafe fn generic_map_aligned(
        hint: *mut u8,
        prot: i32,
        size: isize,
        alignment: isize,
        allocated_size: isize,
        map_flags: i32,
    ) -> *mut u8 {
        let addr = libc::mmap(hint as _, allocated_size as _, prot, map_flags, -1, 0);

        if addr == libc::MAP_FAILED {
            return null_mut();
        }

        let base = addr as usize;
        let aligned_base = round_up(base as _, alignment);

        unmap(base as _, aligned_base as _);
        unmap(
            aligned_base as usize + size as usize,
            base + allocated_size as usize,
        );
        aligned_base as _
    }

    pub unsafe fn unmap(start: usize, end: usize) {
        let size = end - start;
        if size == 0 {
            return;
        }

        if libc::munmap(start as _, size) != 0 {
            panic!("munmap error");
        }
    }
    #[allow(dead_code)]
    pub unsafe fn map_aligned(
        hint: *mut u8,
        fd: i32,
        prot: i32,
        size: isize,
        alignment: isize,
        allocated_size: isize,
    ) -> *mut u8 {
        let mut address = libc::mmap(
            hint as _,
            allocated_size as _,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );

        if address == libc::MAP_FAILED {
            return null_mut();
        }

        let base = address as usize;
        let aligned_base = round_up(base as _, alignment);

        address = libc::mmap(
            aligned_base as _,
            size as _,
            prot,
            libc::MAP_SHARED | libc::MAP_FIXED,
            fd,
            0,
        );

        if address == libc::MAP_FAILED {
            unmap(base as _, base as usize + allocated_size as usize);
            return null_mut();
        }

        unmap(base as _, aligned_base as _);
        unmap(
            aligned_base as usize + size as usize,
            base + allocated_size as usize,
        );

        address as _
    }

    impl VirtualMemory {
        pub fn init() {}
        pub fn cleanup() {}

        pub fn allocate_aligned(
            size: usize,
            alignment: usize,
            is_executable: bool,
            is_compressed: bool,
            name: &'static str,
        ) -> Option<Box<Self>> {
            let _ = is_compressed;
            let _ = name;
            let allocated_size = size + alignment - page_size();

            let prot = libc::PROT_READ
                | libc::PROT_WRITE
                | if is_executable && !cfg!(target_vendor = "apple") {
                    libc::PROT_EXEC
                } else {
                    0
                };

            let mut map_flags = libc::MAP_ANONYMOUS | libc::MAP_PRIVATE;

            if is_executable && cfg!(target_vendor = "apple") {
                map_flags |= libc::MAP_JIT;
            }

            let mut hint = null_mut();

            if is_executable {
                hint = null_mut();
            }
            unsafe {
                let address = generic_map_aligned(
                    hint,
                    prot,
                    size as _,
                    alignment as _,
                    allocated_size as _,
                    map_flags,
                );

                if address == null_mut() {
                    return None;
                }

                let region = MemoryRegion::new(address, size);

                Some(Box::new(VirtualMemory::new(region, region, region)))
            }
        }

        pub fn reserve(size: usize, alignment: usize) -> Option<Box<Self>> {
            let allocated_size = size + alignment - page_size();

            let address = unsafe {
                generic_map_aligned(
                    null_mut(),
                    libc::PROT_NONE,
                    size as _,
                    alignment as _,
                    allocated_size as _,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                )
            };

            if address.is_null() {
                return None;
            }

            let region = MemoryRegion::new(address, size);

            Some(Box::new(VirtualMemory::new(region, region, region)))
        }

        pub unsafe fn commit(address: *mut u8, size: usize) {
            let result = libc::mmap(
                address as _,
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                -1,
                0,
            );
            if result == libc::MAP_FAILED {
                panic!("failed to commit");
            }
        }

        pub unsafe fn decommit(address: *mut u8, size: usize) {
            let result = libc::mmap(
                address as _,
                size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if result == libc::MAP_FAILED {
                panic!("failed to decommit");
            }
        }

        pub unsafe fn free_sub_segment(address: *mut u8, size: usize) -> bool {
            let start = address as usize;

            unmap(start, start + size);

            true
        }

        pub unsafe fn raw_protect(address: *mut u8, size: usize, mode: Protection) {
            let start_address = address as usize;
            let end_address = start_address + size;
            let page_address = round_down(start_address as _, page_size() as _);

            let prot;

            match mode {
                Protection::NoAccess => prot = libc::PROT_NONE,
                Protection::ReadOnly => prot = libc::PROT_READ,
                Protection::ReadWrite => {
                    prot = libc::PROT_READ | libc::PROT_WRITE;
                }
                Protection::ReadExecute => {
                    prot = libc::PROT_READ | libc::PROT_EXEC;
                }
                Protection::ReadWriteExecute => {
                    prot = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;
                }
            }

            if libc::mprotect(
                page_address as _,
                end_address as usize - page_address as usize,
                prot,
            ) != 0
            {
                panic!(
                    "mprotect(0x{:x}, 0x{:x}, {}) failed",
                    page_address,
                    end_address as isize - page_address as isize,
                    prot
                );
            }
        }

        pub unsafe fn dont_need(address: *mut u8, size: usize) {
            let start_address = address as usize;
            let end_address = start_address + size;
            let page_address = round_down(start_address as _, page_size() as _);

            if libc::madvise(
                page_address as _,
                end_address as usize - page_address as usize,
                libc::MADV_DONTNEED,
            ) != 0
            {
                panic!(
                    "madvise(0x{:x}, 0x{:x}, MADV_DONTNEED) failed",
                    page_address,
                    end_address as isize - page_address as isize
                );
            }
        }
    }

    impl Drop for VirtualMemory {
        fn drop(&mut self) {
            if self.vm_owns_region() {
                unsafe {
                    unmap(self.reserved.start(), self.reserved.end());

                    let alias_offset = self.alias_offset();
                    if alias_offset != 0 {
                        unmap(
                            self.reserved.start() + alias_offset,
                            self.reserved.end() + alias_offset,
                        );
                    }
                }
            }
        }
    }
}

static mut PAGE_SIZE: usize = 0;
static mut PAGE_SIZE_BITS: usize = 0;

pub fn page_size() -> usize {
    let result = unsafe { PAGE_SIZE };

    if result != 0 {
        return result;
    }

    init_page_size();

    unsafe { PAGE_SIZE }
}

pub fn page_size_bits() -> usize {
    let result = unsafe { PAGE_SIZE_BITS };

    if result != 0 {
        return result;
    }

    init_page_size();

    unsafe { PAGE_SIZE_BITS }
}

fn init_page_size() {
    unsafe {
        PAGE_SIZE = determine_page_size();
        assert!((PAGE_SIZE & (PAGE_SIZE - 1)) == 0);

        PAGE_SIZE_BITS = log2(PAGE_SIZE);
    }
}

#[cfg(target_family = "unix")]
fn determine_page_size() -> usize {
    let val = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };

    if val <= 0 {
        panic!("could not determine page size.");
    }

    val as usize
}

#[cfg(target_family = "windows")]
fn determine_page_size() -> usize {
    use winapi::um::sysinfoapi::{GetSystemInfo, LPSYSTEM_INFO, SYSTEM_INFO};

    unsafe {
        let mut system_info: SYSTEM_INFO = std::mem::zeroed();
        GetSystemInfo(&mut system_info as LPSYSTEM_INFO);

        system_info.dwPageSize as usize
    }
}

/// determine log_2 of given value
fn log2(mut val: usize) -> usize {
    let mut log = 0;
    assert!(val <= u32::max_value() as usize);

    if (val & 0xFFFF0000) != 0 {
        val >>= 16;
        log += 16;
    }
    if val >= 256 {
        val >>= 8;
        log += 8;
    }
    if val >= 16 {
        val >>= 4;
        log += 4;
    }
    if val >= 4 {
        val >>= 2;
        log += 2;
    }

    log + (val >> 1)
}

#[test]
fn test_log2() {
    for i in 0..32 {
        assert_eq!(i, log2(1 << i));
    }
}
