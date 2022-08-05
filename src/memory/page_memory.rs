use std::{
    collections::{BTreeMap, HashMap},
    ops::{Deref, DerefMut},
    ptr::null_mut,
};

use crate::base::{
    constants::GUARD_PAGE_SIZE, memory_region::MemoryRegion, virtual_memory::VirtualMemory,
};

use super::page::*;

pub struct PageMemory {
    overall: MemoryRegion,
    writeable: MemoryRegion,
}

impl PageMemory {
    pub const fn writeable(&self) -> &MemoryRegion {
        &self.writeable
    }

    pub const fn overall(&self) -> &MemoryRegion {
        &self.overall
    }

    pub const fn new(overall: MemoryRegion, writeable: MemoryRegion) -> Self {
        Self { overall, writeable }
    }
}

#[repr(C)]
pub struct PageMemoryRegion {
    base: PageMemory,
    memory: Box<VirtualMemory>,
    reserved_region: MemoryRegion,
    is_large: bool,
}

#[repr(C)]
pub struct NormalPageMemoryRegion {
    base: PageMemoryRegion,
    page_memories_in_use: [bool; 10],
}

impl NormalPageMemoryRegion {
    pub fn get_page_memory(&self, index: usize) -> PageMemory {
        PageMemory::new(
            MemoryRegion::new(
                (self.reserved_region.start() + PAGE_SIZE * index) as _,
                PAGE_SIZE,
            ),
            MemoryRegion::new(
                (self.reserved_region.start() + PAGE_SIZE * index + GUARD_PAGE_SIZE) as _,
                PAGE_SIZE - 2 * GUARD_PAGE_SIZE,
            ),
        )
    }
}

impl Deref for NormalPageMemoryRegion {
    type Target = PageMemoryRegion;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for NormalPageMemoryRegion {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

#[repr(C)]
pub struct LargePageMemoryRegion {
    base: PageMemoryRegion,
}

impl LargePageMemoryRegion {
    pub fn get_page_memory(&self) -> PageMemory {
        PageMemory::new(
            MemoryRegion::new(
                self.reserved_region.start() as _,
                self.reserved_region.size(),
            ),
            MemoryRegion::new(
                (self.reserved_region.start() + GUARD_PAGE_SIZE) as _,
                PAGE_SIZE - 2 * GUARD_PAGE_SIZE,
            ),
        )
    }
}

impl Deref for LargePageMemoryRegion {
    type Target = PageMemoryRegion;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for LargePageMemoryRegion {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

pub struct NormalPageMemoryPool {
    pool: [Vec<(*mut NormalPageMemoryRegion, usize)>; 16],
}

impl NormalPageMemoryPool {
    pub fn take(&mut self, bucket: usize) -> Option<(*mut NormalPageMemoryRegion, usize)> {
        self.pool[bucket].pop()
    }

    pub fn add(&mut self, bucket: usize, region: *mut NormalPageMemoryRegion, index: usize) {
        self.pool[bucket].push((region, index));
    }
}

pub struct PageMemoryRegionTree {
    set: BTreeMap<usize, *mut PageMemoryRegion>,
}

impl PageMemoryRegionTree {
    pub fn lookup(&self, addr: usize) -> *mut PageMemoryRegion {
        let mut prev = 0;
        let mut region = null_mut::<PageMemoryRegion>();
        for (ptr, result) in self.set.iter() {
            if *ptr < addr {
                prev = *ptr;
                region = *result;
                continue;
            }
            break;
        }

        if prev == 0 {
            return null_mut();
        }

        unsafe {
            if addr < (*region).reserved_region.end() {
                return region;
            }
            null_mut()
        }
    }
}

impl NormalPageMemoryRegion {
    pub fn get_index(&self, addr: usize) -> usize {
        (addr.wrapping_sub(self.reserved_region.start())).wrapping_shr(PAGE_SIZE_LOG2 as _)
    }

    pub fn lookup(&self, addr: usize) -> usize {
        let index = self.get_index(addr);

        if !self.page_memories_in_use[index] {
            return 0;
        }

        let region = *self.get_page_memory(index).writeable();
        if region.contains(addr) {
            region.start()
        } else {
            0
        }
    }
}

#[allow(dead_code)]
pub struct PageBackend {
    page_pool: NormalPageMemoryPool,
    page_memory_region_tree: PageMemoryRegionTree,
    normal_page_memory_regions: Vec<*mut NormalPageMemoryRegion>,
    large_page_memory_regions: HashMap<usize, *mut PageMemoryRegion>,
}

impl PageBackend {}
