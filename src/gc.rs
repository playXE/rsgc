pub const GC_PAGE_BITS: usize = 16;
pub const GC_PAGE_SIZE: usize = 1 << GC_PAGE_BITS;

cfg_if::cfg_if! {
    if #[cfg(not(target_pointer_width="64"))]
    {
        pub const fn gc_hash(ptr: usize) -> usize {
            ptr
        }

        pub const GC_LEVEL0_BITS: usize = 8;
        pub const GC_LEVEL1_BITS: usize = 8;
    } else {
        pub const GC_LEVEL0_BITS: usize = 10;
        pub const GC_LEVEL1_BITS: usize = 10;

        // we currently discard the higher bits
        // we should instead have some special handling for them
        // in x86-64 user space grows up to 0x8000-00000000 (16 bits base + 31 bits page id)
        cfg_if::cfg_if! {
            if #[cfg(target_os="windows")] {
                pub const fn gc_hash(ptr: usize) -> usize {
                    ptr & 0x0000000FFFFFFFFFusize
                }
            } else {
                // Linux gives addresses using the following patterns (X=any,Y=small value - can be 0):
                //		0x0000000YXXX0000
                //		0x0007FY0YXXX0000
                pub const fn gc_hash(ptr: usize) -> usize {
                    let v = ptr;
                    (v ^ ((v >> 33) << 28)) & 0x0000000FFFFFFFFF
                }
            }
        }
    }
}

pub const GC_MASK_BITS: usize = 16;
pub const PAGE_KIND_BITS: usize = 2;
pub const PAGE_KIND_MASK: usize = (1 << PAGE_KIND_BITS) - 1;
pub const GC_LEVEL1_MASK: usize = (1 << GC_LEVEL1_BITS) - 1;

use std::{
    any::{type_name, TypeId},
    marker::PhantomData,
    mem::size_of,
    ptr::{null_mut},
    thread::ThreadId, cell::Cell,
};

use crate::{mem_has_ptr, MEM_KIND_FINALIZE, MEM_KIND_NOPTR, MEM_KIND_RAW};

pub type FlCursor = u16;
pub struct GcFl {
    pub pos: FlCursor,
    pub count: FlCursor,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GcFreeList {
    pub current: i32,
    pub count: i32,
    pub size_bits: i32,
    pub data: *mut GcFl,
}

impl GcFreeList {
    pub fn new() -> Self {
        Self {
            current: 0,
            count: 0,
            size_bits: 0,
            data: null_mut(),
        }
    }
}

impl GcFreeList {
    pub fn get(&self, pos: usize) -> *mut GcFl {
        unsafe { self.data.add(pos) }
    }
}

#[repr(C)]
pub struct GcAllocatorPageData {
    pub block_size: i32,
    pub max_blocks: i32,
    pub first_block: i32,
    pub need_flush: bool,
    pub free: GcFreeList,
    pub sizes: *mut u8,
    pub sizes_ref: i32,
    pub sizes_ref2: i32,
}

pub const GC_PARTITIONS: usize = 9;
pub const GC_PART_BITS: usize = 4;
pub const GC_FIXED_PARTS: usize = 5;
pub const GC_LARGE_PART: usize = GC_PARTITIONS - 1;
pub const GC_LARGE_BLOCK: usize = 1 << 20;
pub const GC_SBITS: [usize; GC_PARTITIONS] = [0, 0, 0, 0, 0, 3, 6, 13, 0];
pub static GC_SIZES: [usize; GC_PARTITIONS] = [8, 16, 24, 32, 40, 8, 64, 1 << 13, 0];
pub const GC_ALIGN_BITS: usize = 3;
pub const GC_ALL_PAGES: usize = GC_PARTITIONS << PAGE_KIND_BITS;
pub const GC_ALIGN: usize = 1 << GC_ALIGN_BITS;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CachedSlot {
    count: i32,
    data: [*mut GcFl; 16],
}

impl CachedSlot {
    pub fn new() -> Self {
        Self {
            count: 0,
            data: [null_mut(); 16],
        }
    }
}

pub struct GcPHeader {
    pub base: *mut u8,
    pub bmp: *mut u8,
    pub page_size: i32,
    pub page_kind: i32,
    pub alloc: GcAllocatorPageData,
    pub next_page: *mut Self,
    #[cfg(debug_assertions)]
    pub page_id: i32,
}

cfg_if::cfg_if! {
    if #[cfg(not(target_os = "windows"))] {
        const BASE_ADDR: usize = 0x40000000;

        struct PExtra {
            page_ptr: *mut u8,
            base_ptr: *mut u8,
            next: *mut PExtra
        }

        pub const EXTRA_SIZE: usize = GC_PAGE_SIZE + (4 << 10);
    }
}

#[derive(Clone, Copy, Default)]
pub struct GcStats {
    pub total_requested: i64,
    pub total_allocated: i64,
    pub last_mark: i64,
    pub last_mark_allocs: i64,
    pub pages_total_memory: i64,
    pub allocation_count: i64,
    pub pages_count: usize,
    pub pages_allocated: usize,
    pub pages_blocks: usize,
    pub mark_bytes: usize,
    pub mark_time: usize,
    pub mark_count: usize,
    pub alloc_time: usize,
}

pub struct GarbageCollector {
    gc_flags: i32,
    gc_level1_null: [*mut GcPHeader; 1 << GC_LEVEL1_BITS],
    gc_page_map: [*mut *mut GcPHeader; 1 << GC_LEVEL0_BITS],
    gc_free_pheaders: *mut GcPHeader,
    gc_pages: [*mut GcPHeader; GC_ALL_PAGES],
    gc_free_pages: [*mut GcPHeader; GC_ALL_PAGES],
    cached_slots: [CachedSlot; 32],
    mark_stack: Vec<*mut u8>,
    mark_size: usize,
    mark_data: *mut u8,
    gc_roots: Vec<*mut *mut u8>,
    free_list_size: usize,
    free_list_count: usize,
    extra_pages: *mut PExtra,
    threads: ThreadsInfo,
    current_thread: thread_local::ThreadLocal<ThreadInfoRef>,
    base_addr: usize,
    recursions: usize,
    stats: GcStats,
    #[cfg(debug_assertions)]
    page_id: usize,
}

impl GarbageCollector {
    pub fn new() -> Box<Self> {
        let mut this = Box::new(Self {
            gc_flags: 0,
            gc_free_pages: [null_mut(); GC_ALL_PAGES],
            gc_pages: [null_mut(); GC_ALL_PAGES],
            gc_free_pheaders: null_mut(),
            gc_page_map: [null_mut(); 1 << GC_LEVEL0_BITS],
            gc_level1_null: [null_mut(); 1 << GC_LEVEL1_BITS],
            cached_slots: [CachedSlot::new(); 32],
            free_list_size: 0,
            free_list_count: 0,
            gc_roots: vec![],
            threads: ThreadsInfo {
                stopping_world: false,
                threads: vec![],
                global_lock: Lock::INIT,
            },
            current_thread: thread_local::ThreadLocal::new(),
            extra_pages: null_mut(),
            base_addr: BASE_ADDR,
            mark_stack: vec![],
            recursions: 0,
            mark_data: null_mut(),
            mark_size: 0,
            stats: GcStats::default(),
            #[cfg(debug_assertions)]
            page_id: 0,
        });

        for i in 0..1 << GC_LEVEL0_BITS {
            this.gc_page_map[i] = this.gc_level1_null.as_mut_ptr();
        }

        this
    }

    fn check_mark(&mut self) {
        let m = self.stats.total_allocated as i64 - self.stats.last_mark;
        let b = self.stats.allocation_count as i64 - self.stats.last_mark_allocs;

        if m as f64 > self.stats.pages_total_memory as f64 * 0.2 || b as f64 > self.stats.pages_blocks as f64 * 0.2 {
            self.major();
        }
    }

    pub fn get_level1(&self, ptr: usize) -> *mut *mut GcPHeader {
        self.gc_page_map[gc_hash(ptr) >> (GC_MASK_BITS + GC_LEVEL1_BITS)]
    }

    pub fn get_page(&self, ptr: usize) -> *mut GcPHeader {
        unsafe {
            self.get_level1(ptr)
                .add((gc_hash(ptr) >> GC_MASK_BITS) & GC_LEVEL1_MASK)
                .read()
        }
    }

    pub fn set_page(&mut self, ptr: usize, page: *mut GcPHeader) {
        unsafe {
            self.get_level1(ptr)
                .add((gc_hash(ptr) >> GC_MASK_BITS) & GC_LEVEL1_MASK)
                .write(page);
        }
    }

    pub fn set_level1(&mut self, ptr: usize, level1: *mut *mut GcPHeader) {
        self.gc_page_map[gc_hash(ptr) >> (GC_MASK_BITS + GC_LEVEL1_BITS)] = level1;
    }

    pub fn will_collide(&self, ptr: usize, size: usize) -> usize {
        #[cfg(target_pointer_width = "64")]
        {
            for i in 0..size >> GC_MASK_BITS {
                let ptr = ptr + (i << GC_MASK_BITS);
                if !self.get_page(ptr).is_null() {
                    return ptr;
                }
            }
        }

        0
    }

    unsafe fn in_page(&self, ptr: usize, page: *mut GcPHeader) -> bool {
        #[cfg(target_pointer_width = "64")]
        {
            ptr as *mut u8 >= (*page).base
                && (ptr as *mut u8) < (*page).base.add((*page).page_size as _)
        }
        #[cfg(target_pointer_width = "32")]
        {
            true
        }
    }

    pub unsafe fn alloc_freelist(&mut self, fl: &mut GcFreeList, size: usize) {
        let slot = &mut self.cached_slots[size];

        if slot.count != 0 {
            slot.count -= 1;
            fl.data = slot.data[slot.count as usize];
            fl.size_bits = size as _;
            fl.count = 0;
            fl.current = 0;
            return;
        }

        let bytes = size_of::<GcFl>() * (1 << size);

        self.free_list_size += bytes;
        self.free_list_count += 1;

        fl.data = libc::malloc(bytes).cast();
        fl.count = 0;
        fl.current = 0;
        fl.size_bits = size as _;
    }

    pub unsafe fn free_freelist(&mut self, fl: &mut GcFreeList) {
        let slot = &mut self.cached_slots[fl.size_bits as usize];
        if slot.count == 16 {
            libc::free(fl.data.cast());
            self.free_list_size -= size_of::<GcFl>() * (1 << fl.size_bits as usize);
            return;
        }

        slot.data[slot.count as usize] = fl.data;
        slot.count += 1;
    }

    pub unsafe fn freelist_append(&mut self, fl: &mut GcFreeList, pos: usize, count: usize) {
        if fl.count == 1 << fl.size_bits as usize {
            debug_assert!(fl.current != 0);

            let mut fl2 = GcFreeList::new();
            self.alloc_freelist(&mut fl2, fl.size_bits as usize + 1);
            core::ptr::copy_nonoverlapping(
                fl.get(0).cast::<u8>(),
                fl2.get(0).cast::<u8>(),
                size_of::<GcFl>() * fl.count as usize,
            );

            self.free_freelist(fl);
            fl.size_bits += 1;
            fl.data = fl2.data;
        }

        let p = fl.get(fl.count as _);
        fl.count += 1;
        (*p).pos = pos as _;
        (*p).count = count as _;
    }

    pub unsafe fn alloc_page_memory(&mut self, size: usize) -> *mut u8 {
        cfg_if::cfg_if! {
            if #[cfg(target_os = "windows")] {
                null_mut()
            } else {

                let mut i = 0;
                while self.will_collide(self.base_addr, size) != 0 {
                    self.base_addr += GC_PAGE_SIZE;
                    i += 1;

                    // most likely hash function creates too many collisions
                    if i >= 1 << (GC_LEVEL0_BITS + GC_LEVEL1_BITS + 2) {
                        return null_mut();
                    }
                }

                let mut ptr = libc::mmap(self.base_addr as _, size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);

                if ptr as isize == -1 {
                    return null_mut();
                }

                if (ptr as usize & (GC_PAGE_SIZE - 1)) != 0 {
                    libc::munmap(ptr as _, size);

                    // recursion limit reached, allocate extra pages to properly align the memory
                    if self.recursions >= 5 {
                        let ptr = libc::mmap(self.base_addr as _, size + EXTRA_SIZE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
                        let offset = (ptr as isize) & (GC_PAGE_SIZE as isize - 1);
                        let aligned = ptr.cast::<u8>().offset(GC_PAGE_SIZE as isize - offset);

                        let inf = if offset > (EXTRA_SIZE as isize >>1) {
                            ptr.cast::<u8>().add(EXTRA_SIZE).sub(size_of::<PExtra>()).cast::<PExtra>()
                        } else {
                            ptr.cast::<PExtra>()
                        };
                        inf.write(PExtra { page_ptr: aligned, base_ptr: ptr as _, next: self.extra_pages });
                        self.extra_pages = inf;
                        return aligned;
                    }

                    let mut tmp;
                    let tmp_size = ptr as isize - self.base_addr as isize;
                    if tmp_size > 0 {
                        self.base_addr = ((ptr as isize & !(GC_PAGE_SIZE as isize - 1)) + GC_PAGE_SIZE as isize) as usize;
                        tmp = ptr;
                    } else {
                        self.base_addr = (ptr as isize & !(GC_PAGE_SIZE as isize - 1)) as usize;
                        tmp = null_mut();
                    }

                    if !tmp.is_null() {
                        tmp = libc::mmap(tmp as _, tmp_size as _, libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
                    }
                    self.recursions += 1;
                    ptr = self.alloc_page_memory(size).cast(); // recurse until properly aligned pages are mmap-ed
                    self.recursions -= 1;

                    if !tmp.is_null() {
                        libc::munmap(tmp as _, tmp_size as _);
                    }

                    return ptr.cast();
                }

                self.base_addr = ptr.cast::<u8>().add(size) as usize;
                ptr.cast()
            }


        }
    }

    pub unsafe fn free_page_memory(&mut self, ptr: usize, size: usize) {
        cfg_if::cfg_if! {
            if #[cfg(target_os="windows")] {

            } else {
                let mut e = self.extra_pages;
                let mut prev: *mut PExtra = null_mut();

                while !e.is_null() {
                    if (*e).page_ptr as usize == ptr {

                        if !prev.is_null() {
                            (*prev).next = (*e).next;
                        } else {
                            self.extra_pages = (*e).next;
                        }

                        if (*e).base_ptr.is_null() {
                            return;
                        }
                        libc::munmap((*e).base_ptr as _, size + EXTRA_SIZE);
                        return;
                    }

                    prev = e;
                    e = (*e).next;
                }
            }
        }
    }

    unsafe fn before_mark(&mut self, mut mark_cur: *mut u8) {
        for pid in 0..GC_ALL_PAGES {
            let mut p = self.gc_pages[pid];
            self.gc_free_pages[pid] = p;

            while !p.is_null() {
                (*p).bmp = mark_cur;
                (*p).alloc.need_flush = true;
                mark_cur = mark_cur.add(((*p).alloc.max_blocks as usize + 7) >> 3);
                p = (*p).next_page;
            }
        }
    }

    unsafe fn flush_empty_pages(&mut self) {
        for i in 0..GC_ALL_PAGES {
            let mut ph = self.gc_pages[i];
            let mut prev: *mut GcPHeader = null_mut();

            while !ph.is_null() {
                let p = &mut (*ph).alloc;
                let next = (*ph).next_page;

                if !(*ph).bmp.is_null()
                    && is_zero(
                        (*ph).bmp.add(p.first_block as usize >> 3),
                        ((p.max_blocks as usize + 7) >> 3) - (p.first_block as usize >> 3),
                    )
                {
                    if !prev.is_null() {
                        (*prev).next_page = next;
                    } else {
                        self.gc_pages[i] = next;
                    }

                    if self.gc_free_pages[i] == ph {
                        self.gc_free_pages[i] = next;
                    }

                    self.free_freelist(&mut p.free);
                
                    self.free_page(ph, p.max_blocks as usize);
                } else {
                    prev = ph;
                }

                ph = next;
            }
        }
    }

    pub unsafe fn free_page(&mut self, ph: *mut GcPHeader, block_count: usize) {
        for i in 0..((*ph).page_size as usize >> GC_MASK_BITS) {
            let ptr = (*ph).base.add(i << GC_MASK_BITS);
            self.set_page(ptr as _, null_mut());
        }

        self.stats.pages_count -= 1;
        self.stats.pages_blocks -= block_count;
        self.stats.pages_total_memory -= (*ph).page_size as i64;
        self.stats.mark_bytes -= (block_count + 7) >> 3;
        self.free_page_memory((*ph).base as _, (*ph).page_size as _);
        (*ph).next_page = self.gc_free_pheaders;
        self.gc_free_pheaders = ph;
    }

    pub unsafe fn alloc_page(
        &mut self,
        size: usize,
        kind: i32,
        block_count: usize,
    ) -> *mut GcPHeader {
        let base = self.alloc_page_memory(size);

        if base.is_null() {
            let pages = self.stats.pages_allocated;
            self.major();

            if pages != self.stats.pages_allocated {
                return self.alloc_page(size, kind, block_count);
            }

            if size >= (8 << 20) {
                std::panic::panic_any(FailedAlloc(size >> 10));
            }

            eprintln!("out of memory: pages");
            std::process::abort();
        }

        let mut p = self.gc_free_pheaders;
        if p.is_null() {
            let head = libc::malloc(size_of::<GcPHeader>() * 100) as *mut GcPHeader;
            p = head;

            for i in 1..99 {
                (*p).next_page = head.add(i);
                p = (*p).next_page;
            }

            (*p).next_page = null_mut();
            self.gc_free_pheaders = head;
            p = self.gc_free_pheaders;
        }

        self.gc_free_pheaders = (*p).next_page;
        core::ptr::write_bytes(p, 0, 1);
        (*p).base = base;
        (*p).page_size = size as _;

        #[cfg(debug_assertions)]
        {
            core::ptr::write_bytes(base, 0xdd, size);
            (*p).page_id = self.page_id as _;
            self.page_id += 1;
        }
        #[cfg(not(debug_assertions))]
        {
            if kind == super::MEM_KIND_DYNAMIC {
                core::ptr::write_bytes(base, 0x0, size);
            }
        }

        if ((base as usize) & ((1 << GC_MASK_BITS) - 1)) != 0 {
            eprintln!("page memory is not correctly aligned");
            std::process::abort();
        }

        (*p).page_size = size as _;
        (*p).page_kind = kind;
        (*p).bmp = null_mut();

        self.stats.pages_count += 1;
        self.stats.pages_blocks += block_count;
        self.stats.pages_allocated += 1;
        self.stats.pages_total_memory += size as i64;
        self.stats.mark_bytes += (block_count + 7) >> 3;

        for i in 0..size >> GC_MASK_BITS {
            let ptr = (*p).base.add(i << GC_MASK_BITS);

            if self.get_level1(ptr as _) == self.gc_level1_null.as_mut_ptr() {
                let level = libc::malloc(size_of::<*mut u8>() * (1 << GC_LEVEL1_BITS))
                    .cast::<*mut GcPHeader>();

                core::ptr::write_bytes(level, 0, 1 << GC_LEVEL1_BITS);
                self.set_level1(ptr as _, level);
            }

            self.set_page(ptr as _, p);
        }

        p
    }

    unsafe fn flush_free_list(&mut self, ph: *mut GcPHeader) {
        let p = &mut (*ph).alloc;

        let mut bid = p.first_block;
        let last = p.max_blocks;

        let mut new_fl = GcFreeList::new();
        self.alloc_freelist(&mut new_fl, p.free.size_bits as _);

        let mut old_fl = p.free;

        let mut cur_pos: *mut GcFl = null_mut();
        let mut reuse_index = old_fl.current;
        let mut reuse = if reuse_index < old_fl.count {
            let reuse = old_fl.get(reuse_index as _);
            reuse_index += 1;
            reuse
        } else {
            null_mut()
        };

        let mut next_bid = if !reuse.is_null() {
            (*reuse).pos as i32
        } else {
            -1
        };

        let bmp = (*ph).bmp;

        while bid < last {
            if bid == next_bid {
                if !cur_pos.is_null() && (*cur_pos).pos as i32 + (*cur_pos).count as i32 == bid {
                    (*cur_pos).count += (*reuse).count;
                } else {
                    self.freelist_append(&mut new_fl, (*reuse).pos as _, (*reuse).count as _);
                    cur_pos = new_fl.get(new_fl.count as usize - 1);
                }

                reuse = if reuse_index < old_fl.count {
                    let reuse = old_fl.get(reuse_index as _);
                    reuse_index += 1;
                    reuse
                } else {
                    null_mut()
                };
                bid = (*cur_pos).count as i32 + (*cur_pos).pos as i32;
                next_bid = if !reuse.is_null() {
                    (*reuse).pos as i32
                } else {
                    -1
                };
            }

            let mut count: FlCursor;
            if !(*p).sizes.is_null() {
                count = (*p).sizes.add(bid as _).read() as _;
                if count == 0 {
                    count = 1;
                }
            } else {
                count = 1;
            }

            if (bmp.add(bid as usize >> 3).read() & (1 << (bid & 7))) == 0 {
                if !(*p).sizes.is_null() {
                    (*p).sizes.add(bid as _).write(0);
                }

                if !cur_pos.is_null() && (*cur_pos).count as i32 + (*cur_pos).pos as i32 == bid {
                    (*cur_pos).count += count;
                } else {
                    self.freelist_append(&mut new_fl, bid as _, count as _);
                    cur_pos = new_fl.get(new_fl.count as usize - 1);
                }
            }

            bid += count as i32;
        }

        (*p).free = new_fl;
        (*p).need_flush = false;

        self.free_freelist(&mut old_fl);
    }

    unsafe fn new_page(
        &mut self,
        pid: usize,
        block: usize,
        mut size: usize,
        kind: i32,
        varsize: bool,
    ) -> *mut GcPHeader {
        if block < 256 {
            let mut num_pages = 0;
            let mut ph = self.gc_pages[pid];

            while !ph.is_null() {
                num_pages += 1;
                ph = (*ph).next_page;
            }

            while num_pages > 8 && (size << 1) / block <= GC_PAGE_SIZE {
                size <<= 1;
                num_pages /= 3;
            }
        }

        let mut start_pos = 0;
        let max_blocks = size / block;

        let mut ph = self.alloc_page(size, kind, max_blocks as _);
        let p = &mut (*ph).alloc;

        p.block_size = block as _;
        p.max_blocks = max_blocks as _;
        p.sizes = null_mut();

        if p.max_blocks > GC_PAGE_SIZE as i32 {
            panic!("too many blocks for this page");
        }

        if varsize {
            if p.max_blocks <= 8 {
                p.sizes = &p.sizes_ref as *const i32 as _;
            } else {
                p.sizes = (*ph).base.add(start_pos as _);
                start_pos += p.max_blocks as i32;
                start_pos += (-start_pos) & 63;
            }

            core::ptr::write_bytes(p.sizes, 0, p.max_blocks as _);
        }

        let m = start_pos % block as i32;
        if m != 0 {
            start_pos += block as i32 - m;
        }

        let mut fl_bits = 1;
        while fl_bits < 8 && (1 << fl_bits) < (p.max_blocks >> 3) {
            fl_bits += 1;
        }

        p.first_block = start_pos / block as i32;
        self.alloc_freelist(&mut p.free, fl_bits as _);
        self.freelist_append(
            &mut p.free,
            p.first_block as _,
            p.max_blocks as usize - p.first_block as usize,
        );

        p.need_flush = false;

        (*ph).next_page = self.gc_pages[pid];
        self.gc_pages[pid] = ph;
        ph
    }

    pub unsafe fn alloc_fixed(&mut self, part: usize, kind: i32) -> *mut u8 {
        let pid = (part << PAGE_KIND_BITS) | kind as usize;

        let mut ph = self.gc_free_pages[pid];
        let mut p = null_mut();

        let mut bid = usize::MAX;

        while !ph.is_null() {
            p = &mut (*ph).alloc;

            if (*p).need_flush {
                self.flush_free_list(ph);
            }

            let fl = &mut (*p).free;

            if fl.current < fl.count {
                let c = fl.get(fl.current as _);
                bid = (*c).pos as _;
                (*c).pos += 1;
                (*c).count -= 1;

                if (*c).count == 0 {
                    (*fl).current += 1;
                }
                break;
            }

            ph = (*ph).next_page;
        }

        if ph.is_null() {
            ph = self.new_page(pid, GC_SIZES[part], GC_PAGE_SIZE, kind, false);
            p = &mut (*ph).alloc;
            bid = (*(*p).free.data).pos as _;
            (*(*p).free.data).pos += 1;
            (*(*p).free.data).count -= 1;
        }

        let ptr = (*ph).base.add(bid * (*p).block_size as usize);
        self.gc_free_pages[pid] = ph;
        ptr
    }

    pub unsafe fn alloc_var(&mut self, part: usize, size: usize, kind: i32) -> *mut u8 {
        let pid = (part << PAGE_KIND_BITS) | kind as usize;

        let mut ph = self.gc_free_pages[pid];
        let mut p = null_mut();

        let ptr;
        let nblocks = size >> GC_SBITS[part];
        let mut bid = usize::MAX;

        'alloc_var: loop {
            while !ph.is_null() {
                p = &mut (*ph).alloc;

                if (*p).need_flush {
                    self.flush_free_list(ph);
                }
                let fl = &mut (*p).free;

                for k in fl.current..fl.count {
                    let c = &mut *fl.get(k as _);

                    if c.count >= nblocks as _ {
                        bid = c.pos as _;
                        c.count -= nblocks as u16;

                        if c.count == 0 {
                            fl.current += 1;
                        }

                        break 'alloc_var;
                    }
                }

                ph = (*ph).next_page;
            }

            if ph.is_null() {
                let mut psize = GC_PAGE_SIZE;

                while psize < size + 1024 {
                    psize <<= 1;
                }

                ph = self.new_page(pid, GC_SIZES[part], psize, kind, true);
                p = &mut (*ph).alloc;

                bid = (*p).first_block as _;
                (*(*p).free.data).pos += nblocks as u16;
                (*(*p).free.data).count -= nblocks as u16;
            }

            break 'alloc_var;
        }

        ptr = (*ph).base.add(bid * (*p).block_size as usize);

        if !(*ph).bmp.is_null() {
            *(*ph).bmp.add(bid >> 3) |= (1 << (bid & 7)) as u8;
        }

        if nblocks > 1 {
            core::ptr::write_bytes((*p).sizes.add(bid), 0, nblocks);
        }

        (*p).sizes.add(bid).write(nblocks as _);
        self.gc_free_pages[pid] = ph;
        ptr
    }

    pub unsafe fn raw_alloc(&mut self, size: &mut isize, page_kind: i32) -> *mut u8 {
        let mut sz = *size as isize;
        sz += (-sz) & (GC_ALIGN as isize - 1);

        if sz >= GC_LARGE_BLOCK as isize {
            sz += (-sz) & (GC_PAGE_SIZE as isize - 1);
            *size = sz as _;
            let ph = self.new_page(
                (GC_LARGE_PART << PAGE_KIND_BITS) | page_kind as usize,
                sz as _,
                sz as _,
                page_kind,
                false,
            );
            return (*ph).base;
        }

        if sz <= GC_SIZES[GC_FIXED_PARTS - 1] as isize && page_kind != MEM_KIND_FINALIZE {
            let part = (sz >> GC_ALIGN_BITS as isize) - 1;
            *size = GC_SIZES[part as usize] as isize;
            return self.alloc_fixed(part as _, page_kind);
        }

        for p in GC_FIXED_PARTS..GC_PARTITIONS {
            let block = GC_SIZES[p];
            let query = sz + ((-sz) & (block as isize - 1));

            if query < block as isize * 255 {
                *size = query;

                return self.alloc_var(p, query as _, page_kind);
            }
        }

        *size = -1;
        return null_mut();
    }

    unsafe fn global_lock(&mut self, lock: bool) {
        let _ = lock;
        #[cfg(feature = "threads")]
        {
            let t = self.current_thread();
            if t.is_null() && self.threads.threads.len() == 0 {
                panic!("Can't lock in GC unregistered thread");
            }
            if lock {
                Self::save_context(t);

                (*t).gc_blocking += 1;

                self.threads.global_lock.lock();
            } else {
                (*t).gc_blocking -= 1;
                self.threads.global_lock.unlock();
            }
        }
    }

    #[inline(never)]
    unsafe fn _major(&mut self) {
        let time = std::time::Instant::now();

        self.stop_world(true);
        self.stats.last_mark = self.stats.total_allocated;
        self.stats.last_mark_allocs = self.stats.allocation_count;
        self.mark();
        self.stop_world(false);

        let _ = time;
    }

    #[inline(never)]
    pub fn major(&mut self) {
        unsafe {
            self.global_lock(true);
            self._major();
            self.global_lock(false);
        }
    }

    unsafe fn get_block_interior(&mut self, page: *mut GcPHeader, block: *mut *mut u8) -> isize {
        let offset = (*block) as isize - (*page).base as isize;
        let mut bid = offset / (*page).alloc.block_size as isize;

        if !(*page).alloc.sizes.is_null() {
            if bid < (*page).alloc.first_block as isize {
                return -1;
            }

            while (*page).alloc.sizes.add(bid as _).read() == 0 {
                if bid == (*page).alloc.first_block as isize {
                    return -1;
                }

                bid -= 1;
            }
        }
    
        *block = (*page).base.offset(bid * (*page).alloc.block_size as isize);
        bid
    }
    #[inline]
    unsafe fn fast_block_size(page: *mut GcPHeader, block: *mut *mut u8) -> usize {
        if !(*page).alloc.sizes.is_null() {
            (*page)
                .alloc
                .sizes
                .add((block as usize - (*page).base as usize) / (*page).alloc.block_size as usize)
                .read() as usize
                * (*page).alloc.block_size as usize
        } else {
            (*page).alloc.block_size as usize
        }
    }

    unsafe fn push(&mut self, page: *mut GcPHeader, ptr: *mut u8) {

        if mem_has_ptr((*page).page_kind) {
            self.mark_stack.push(ptr);
        }
    }

    unsafe fn mark_stack(&mut self, start: *mut u8, end: *mut u8) {
        let mut stack_head = start.cast::<*mut u8>();

        while stack_head < end.cast() {
            let mut p = stack_head.read();
            stack_head = stack_head.add(1);
            
            let page = self.get_page(p as _);
            if page.is_null() || !self.in_page(p as _, page) {
                continue;
            }

            let bid = self.get_block_interior(page, &mut p);

            if bid >= 0 && (*page).bmp.offset(bid >> 3).read() & (1 << (bid & 7)) == 0 {
                *(*page).bmp.offset(bid >> 3) |= 1 << (bid & 7) as u8;
                println!("live {:p} {} {}", p,bid, (*page).bmp.offset(bid >> 3).read() & (1 << (bid & 7)));
                self.push(page, p);
            }
        }
    }

    unsafe fn flush_mark(&mut self) {
        while let Some(block) = self.mark_stack.pop() {
            let mut block = block.cast::<*mut u8>();

            let page = self.get_page(block as _);
            let size = Self::fast_block_size(page, block);
            let nwords = size / size_of::<usize>();

            let mut pos = 0;
            println!("flush mark: {:p} {} words", block, nwords);
            while pos < nwords {
                let mut p = block.read();
                block = block.add(1);
                pos += 1;
                
                let page = self.get_page(p as _);

                if page.is_null() || !self.in_page(p as _, page) {
                    continue;
                }
                println!("check {:p}", p);
                let bid = self.get_block_interior(page, &mut p);
                if bid >= 0 && (*page).bmp.offset(bid >> 3).read() & (1 << (bid & 7)) == 0 {
                    *(*page).bmp.offset(bid >> 3) |= 1 << (bid & 7) as u8;
                    println!("pushing {:?}", p);
                    self.push(page, p);
                }
            }
        }
    }

    unsafe fn mark(&mut self) {
        let mark_bytes = self.stats.mark_bytes;

        if mark_bytes > self.mark_size {
            if !self.mark_data.is_null() {
                self.free_page_memory(self.mark_data as _, self.mark_size);
            }

            if self.mark_size == 0 {
                self.mark_size = GC_PAGE_SIZE;
            }

            while self.mark_size < mark_bytes {
                self.mark_size <<= 1;
            }

            self.mark_data = self.alloc_page_memory(self.mark_size);

            if self.mark_data.is_null() {
                out_of_memory("mark bits");
            }
        }

        core::ptr::write_bytes(self.mark_data, 0, self.mark_size);
        self.before_mark(self.mark_data);

        for i in 0..self.gc_roots.len() {
            let p = *self.gc_roots[i];
            if p.is_null() {
                continue;
            }
            let page = self.get_page(p as _);

            if page.is_null() || !self.in_page(p as _, page) {
                continue;
            }

            let bid = self.get_block_interior(page, p as _);
            if bid >= 0 && (*page).bmp.offset(bid >> 3).read() & (1 << (bid & 7)) == 0 {
                *(*page).bmp.offset(bid >> 3) |= 1 << (bid & 7) as u8;
                self.push(page, p);
            }
        }

        for i in 0..self.threads.threads.len() {
            let th = self.threads.threads[i];

            let approx_sp = (*th).stack_cur;
            let bounds = (*th).stack_bounds;

            let mut start = bounds.origin;
            let mut end = approx_sp;

            if start > end {
                std::mem::swap(&mut start, &mut end);
            }

            self.mark_stack(start, end);
        }

        self.flush_mark();
        self.flush_empty_pages();
    }

    unsafe fn stop_world(&mut self, stop: bool) {
        #[cfg(feature = "threads")]
        {
            if stop {
                std::ptr::write_volatile(&mut self.threads.stopping_world, true);

                for i in 0..self.threads.threads.len() {
                    let th = self.threads.threads[i];

                    if (*th).gc_blocking == 0 {
                        std::hint::spin_loop();
                    }
                }
            } else {
                std::ptr::write_volatile(&mut self.threads.stopping_world, false);
            }
        }

        #[cfg(not(feature = "threads"))]
        {
            Self::save_context(self.current_thread());
        }
    }

    unsafe fn current_thread(&self) -> *mut ThreadInfo {
        self.current_thread
            .get()
            .map(|x| x.0.get())
            .unwrap_or(std::ptr::null_mut())
    }

    unsafe fn save_context(th: *mut ThreadInfo) {
        (*th).stack_cur = approximate_stack_pointer() as _;
    }


    pub unsafe fn register_thread(&mut self) {
        if !self.current_thread().is_null() {
            panic!("Thread already registered");
        }

        let th = Box::into_raw(Box::new(ThreadInfo {
            thread_id: std::thread::current().id(),
            stack_bounds: StackBounds::current_thread_stack_bounds(),
            stack_cur: approximate_stack_pointer() as _,
            gc_blocking: 0,
        }));
        let _ = self.current_thread.get_or(|| ThreadInfoRef(Cell::new(th)));

        self.global_lock(true);
        self.threads.threads.push(th);
        self.global_lock(false);
    }

    pub unsafe fn unregister_thread(&mut self) {
        let t = self.current_thread();
        self.global_lock(true);

        self.current_thread.get().unwrap().0.set(null_mut());
        self.threads.threads.retain(|th| {
            if *th == t {
                false 
            } else {
                true
            }
        });
        self.threads.global_lock.unlock();
    }

    pub unsafe fn alloc_gen(&mut self, _t: Option<&'static Type>, size: usize, flags: i32) -> *mut u8 {

        self.global_lock(true);
        self.check_mark();
        let mut allocated = size as isize;
        let ptr = self.raw_alloc(&mut allocated, flags & PAGE_KIND_MASK as i32);

        self.stats.total_allocated += allocated as i64;
        self.global_lock(false);

        ptr
    }
}

#[inline(never)]
pub fn approximate_stack_pointer() -> *const *const u8 {
    let mut x: *const *const u8 = core::ptr::null();
    x = &x as *const *const *const u8 as *const *const u8;
    x
}

pub unsafe fn is_zero(mut ptr: *const u8, mut size: usize) -> bool {
    static ZEROMEM: [u8; 256] = [0; 256];

    while size >> 8 != 0 {
        if libc::memcmp(ptr.cast(), ZEROMEM.as_ptr().cast(), 256) != 0 {
            return false;
        }

        ptr = ptr.add(256);
        size -= 256;
    }

    libc::memcmp(ptr.cast(), ZEROMEM.as_ptr().cast(), size) == 0
}

pub struct FailedAlloc(usize);

pub struct Marker<'a>(&'a mut GarbageCollector);

/// GC RTTI
pub struct Type {
    pub no_ptr: bool,
    pub trace: Option<fn(*const (), &mut Marker)>,
    pub finalizer: Option<fn(*mut ())>,
    pub name: &'static str,
    pub type_id: TypeId,
}

pub trait Allocation {
    const TRACE: Option<fn(&Self, &mut Marker)>;
    const NO_PTR: bool = false;
    const PAGE_KIND: i32 = if Self::NO_PTR {
        if std::mem::needs_drop::<Self>() {
            MEM_KIND_FINALIZE
        } else {
            MEM_KIND_NOPTR
        }
    } else {
        MEM_KIND_RAW
    };
}

pub struct VT<T>(PhantomData<T>);

pub trait ConstVal<T> {
    const VAL: T;
}

impl<T: 'static + Allocation> ConstVal<&'static Type> for VT<T> {
    const VAL: &'static Type = &Type {
        no_ptr: T::NO_PTR,
        finalizer: if std::mem::needs_drop::<T>() {
            fn erased<T>(x: *mut ()) {
                unsafe {
                    core::ptr::drop_in_place(x.cast::<T>());
                }
            }

            Some(erased::<T>)
        } else {
            None
        },
        trace: if let Some(_) = T::TRACE {
            fn erased<T: Allocation>(p: *const (), marker: &mut Marker) {
                unsafe {
                    let val = &*p.cast::<T>();

                    if let Some(t) = T::TRACE {
                        t(val, marker);
                    }
                }
            }
            Some(erased::<T>)
        } else {
            None
        },

        name: type_name::<T>(),
        type_id: TypeId::of::<T>(),
    };
}

pub struct ThreadInfo {
    thread_id: ThreadId,
    gc_blocking: i32,
    stack_bounds: StackBounds,
    stack_cur: *mut u8,
}

use parking_lot::{
    lock_api::{RawMutex, RawMutexFair},
    RawMutex as Lock,
};

pub struct ThreadsInfo {
    stopping_world: bool,
    threads: Vec<*mut ThreadInfo>,
    global_lock: Lock,
}

pub struct ThreadInfoRef(Cell<*mut ThreadInfo>);

unsafe impl Send for ThreadInfoRef {}
unsafe impl Sync for ThreadInfoRef {}

pub fn out_of_memory(errmsg: &str) {
    std::panic::always_abort();
    panic!("{}", errmsg);
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
