use std::{
    alloc::Layout, collections::BTreeMap, intrinsics::{unlikely, likely}, mem::size_of, ptr::null_mut,
};

use crate::{object::{HeapObjectHeader, VtableTag, CellState}, weak_random::WeakRandom};

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
#[repr(u64)]
pub enum MemoryType {
    Arena = 0,
    Large,
}

#[repr(C)]
pub struct BaseMemory {
    pub typ: MemoryType,
}

impl BaseMemory {
    pub unsafe fn payload(&self) -> *mut u8 {
        let ptr = self as *const Self;
        if self.typ == MemoryType::Arena {
            (*ptr.cast::<Arena>()).base()
        } else {
            ptr as _
        }
    }
}

#[repr(C)]
pub struct LargeObject {
    base: BaseMemory,
    payload_size: usize,
}

impl LargeObject {
    pub const fn page_header_size() -> usize {
        round_up(
            (size_of::<Self>() as isize + size_of::<HeapObjectHeader>() as isize)
                - size_of::<HeapObjectHeader>() as isize,
            32,
        ) as _
    }

    pub const fn allocation_size(size: usize) -> usize {
        Self::page_header_size() + size
    }

    pub unsafe fn payload_start(&self) -> *mut u8 {
        let ptr = self as *const Self as *mut u8;
        ptr.add(Self::page_header_size())
    }

    pub unsafe fn payload_end(&self) -> *mut u8 {
        let ptr = self as *const Self as *mut u8;
        ptr.add(self.payload_size)
    }

    pub unsafe fn from_object(obj: *mut u8) -> *mut Self {
        let ptr = obj as *mut u8;
        ptr.sub(Self::page_header_size()).cast::<Self>()
    }

    pub unsafe fn create(size: usize) -> *mut Self {
        let ptr = std::alloc::alloc(Layout::from_size_align_unchecked(
            Self::allocation_size(size),
            32,
        )) as *mut Self;
        ptr.write(Self {
            base: BaseMemory {
                typ: MemoryType::Large,
            },
            payload_size: size,
        });
        ptr
    }
}

#[repr(C)]
pub struct Arena {
    base_: BaseMemory,
    base: *mut u8,
    end: *mut u8,
    bmp: ObjectStartBitmap,
    nfreepages: usize,
    totalpages: usize,
    freepages: usize,
    npages: usize,
    nextarena: *mut Arena,
}

#[inline(always)]
const fn start_of_page(addr: usize, page_size: usize) -> usize {
    let offset = addr.wrapping_rem(page_size);
    addr.wrapping_sub(offset)
}

impl Arena {
    pub unsafe fn new(arena_size: usize, page_size: usize) -> *mut Arena {
        let arena_base =
            std::alloc::alloc(Layout::from_size_align_unchecked(arena_size, page_size));

        if arena_base.is_null() {
            out_of_memory("out of memory: could not allocate the next arena");
        }

        let arena_end = arena_base.add(arena_size);
        let firstpage = start_of_page(arena_base.add(page_size).sub(1) as usize, page_size);
        println!("firstpage: {:p}", firstpage as *mut u8);
        println!("arena start: {:p} (offset from first page: {})", arena_base, firstpage - arena_base as usize);
        let npages = (arena_end as usize - firstpage) / page_size;

        Box::into_raw(Box::new(Arena {
            base_: BaseMemory {
                typ: MemoryType::Arena,
            },
            base: arena_base,
            end: arena_end,
            bmp: ObjectStartBitmap::new(arena_base as _, arena_size),
            nextarena: null_mut(),
            nfreepages: 0,
            npages,
            totalpages: npages as _,
            freepages: firstpage,
        }))
    }

    pub fn find_object_start(&self, p: *mut u8) -> *mut u8 {
        self.bmp.find_object_start(p as _)
    }

    pub fn set_object_start(&mut self, p: *mut u8) {
        self.bmp.set_bit(p as _);
    }

    pub fn clear_object_start(&mut self, p: *mut u8) {
        self.bmp.clear_bit(p as _);
    }

    pub fn clear_bitmap(&mut self) {
        self.bmp.clear();
    }

    pub fn clear_range(&mut self, start: *mut u8, end: *mut u8) {
        self.bmp.clear_range(start as _, end as _);
    }

    pub fn contains(&self, addr: *const u8) -> bool {
        addr >= self.base && addr < self.end
    }

    pub fn base(&self) -> *mut u8 {
        self.base
    }

    pub fn end(&self) -> *mut u8 {
        self.end
    }

    pub fn size(&self) -> usize {
        self.end as usize - self.base as usize
    }
}

#[repr(C)]
pub struct Page {
    nextpage: *mut Page,
    arena: *mut Arena,
}

impl Page {
    pub fn payload_start(&self) -> usize {
        self as *const Self as _
    }

    unsafe fn specialized_sweep(&mut self, mut free_list: Option<&mut FreeList>, is_empty: bool, ac: &mut ArenaCollection, block_size: usize) -> bool {
        if is_empty {
            let payload_end = self.payload_start() + ac.page_size;
            let payload_begin = self.payload_start() + size_of::<Self>();

            if let Some(free_list) = free_list {
                free_list.initialize_bump(payload_begin as _, payload_end as usize - payload_begin as usize);
            }

            return true;
        }

        let mut head = null_mut::<FreeCell>();
        let mut count = 0;

        let secret = ac.random.get_u64() as usize;

        let mut is_empty = true;

        let mut handle_dead_cell = |obj: *mut u8| {
            if let Some(_) = free_list.as_mut() {
                let cell = obj as *mut FreeCell;

                (*cell).set_next(head, secret);
                head = cell;
                count += 1;
            }
        };

        let start = self.payload_start() + size_of::<Self>();
        let end = self.payload_start() + ac.page_size;

        let mut current = start;
        let bmp = &mut (*self.arena).bmp;
        while current < end {
            if bmp.check_bit(current) {
                let hdr = current as *mut HeapObjectHeader;
                if (*hdr).cell_state() == ac.sweep_color {
                    handle_dead_cell(hdr.cast());
                    bmp.clear_bit(current);
                } else {
                    is_empty = false;
                }
                
                current += block_size;
                continue;
            } else {
                handle_dead_cell(current as _);
            }

            current += block_size;
        }

        if let Some(free_list) = free_list {
            free_list.initialize_list(head, secret, count * block_size);
        }

        is_empty
    }
}

const _X: [u8; 0] = {
    assert!(size_of::<Page>() == size_of::<[usize; 2]>());
    []
};
pub struct ArenaCollection {

    current_arena: *mut Arena,
    arena_size: usize,
    page_size: usize,
    small_request_threshold: usize,
    arenas_count: usize,
    size_class_for_size_step: Vec<usize>,
    nblocks_for_size: Box<[usize]>,
    max_pages_per_arena: usize,
    arenas_lists: Box<[*mut Arena]>,
    old_arenas_lists: Box<[*mut Arena]>,
    min_empty_nfreepages: usize,
    num_uninitialized_pages: isize,
    footprint_limit: usize,
    bytes_allocated_this_cycle: usize,
    tree: ArenaTree,
    sweep_color: CellState,
    large_objects: Vec<*mut u8>,
    random: WeakRandom,
}

impl ArenaCollection {
    pub const fn index_to_size_class(index: usize) -> usize {
        index * Self::WORD
    }

    pub const fn size_class_to_index(size: usize) -> usize {
        (size + Self::WORD - 1) / Self::WORD
    }

    pub const fn num_size_classes(small_request_threshold: usize) -> usize {
        small_request_threshold / Self::WORD + 1
    }

    pub fn build_size_class_table(table: &mut Vec<usize>, size_classes: &[usize], small_request_threshold: usize) {
        let mut next_index = 0;
        for size_class in size_classes.iter().copied() {
            let index = Self::size_class_to_index(size_class);
            for i in next_index..=index {
                table[i] = size_class;
            }

            next_index = index + 1;
        }

        for i in next_index..Self::num_size_classes(small_request_threshold) {
            table[i] = Self::index_to_size_class(i);
        }
    } 

    pub fn size_classes(
        dump_size_classes: bool,
        small_request_threshold: usize,
        page_size: usize,
        size_class_progression: f64,
    ) -> Vec<usize> {
        let mut result = Vec::<usize>::new();
        
        if dump_size_classes {
            println!(
                "Page size: {}\nHeader size: {}",
                page_size,
                size_of::<Page>()
            );
        }

        let add = |result: &mut Vec<usize>, size_class: usize| {
            let size_class = round_up(size_class as _, Self::WORD as _) as usize;
            if dump_size_classes {
                println!("Adding size class: {}", size_class);
            }
            result.push(size_class);
        };

        let mut size = Self::WORD;

        while size < 80 {
            add(&mut result, size);
            size += Self::WORD;
        }

        for i in 0.. {
            let approximate_size = 80.0 * size_class_progression.powi(i as _);

            if dump_size_classes {
                println!("  Next size class as a double: {}", approximate_size);
            }

            let approximate_size_in_bytes = approximate_size as usize;

            if dump_size_classes {
                println!("  Next size class as bytes: {}", approximate_size_in_bytes);
            }

            if approximate_size_in_bytes > small_request_threshold {
                break;
            }

            let size_class = round_up(approximate_size_in_bytes as _, Self::WORD as _) as usize;
            if dump_size_classes {
                println!("  Size class: {}", size_class);
            }

            let blocks_per_page = (page_size - size_of::<Page>()) / size_class;
            let possibly_better_size_class = (page_size - size_of::<Page>()) & !(Self::WORD - 1);

            if dump_size_classes {
                println!(
                    "  Possibly better size class: {}",
                    possibly_better_size_class
                );
            }

            let original_wastage = (page_size - size_of::<Page>()) - blocks_per_page * size_class;
            let new_wastage = (possibly_better_size_class - size_class) * blocks_per_page;

            if dump_size_classes {
                println!(
                    "  Original wastage: {}, new wastage: {}",
                    original_wastage, new_wastage
                );
            }

            let better_size_class = if new_wastage > original_wastage {
                size_class
            } else {
                possibly_better_size_class
            };

            if dump_size_classes {
                println!("  Choosing size class: {}", better_size_class);
            }

            if better_size_class == result.last().copied().unwrap_or(usize::MAX) {
                continue;
            }

            if better_size_class > small_request_threshold {
                break;
            }
            add(&mut result, better_size_class);
        }

        add(&mut result, 256);

        result.sort();
        result.dedup();
        
        if dump_size_classes {
            println!("Heap size class dump: {:?}", result);
        }

        result
    }

    pub const WORD: usize = 16;
    pub const WORD_POWER_2: usize = 4;

    pub fn new(arena_size: usize, page_size: usize, small_request_threshold: usize, footprint_limit: usize,) -> Self {
        assert!(arena_size >= (page_size * 2), "Arena size must be at least twice the page size");
        assert!(arena_size % page_size == 0, "Arena size must be a multiple of the page size");
        assert!(small_request_threshold <= page_size / 2, "Small request threshold must be at most half the page size");
        let length = small_request_threshold / Self::WORD + 1;

        let mut nblocks_for_size = vec![0; length].into_boxed_slice();

        nblocks_for_size[0] = 0; // unused

        for i in 1..length {
            nblocks_for_size[i] = (page_size - size_of::<Page>()) / (Self::WORD * i);
        }

        let max_pages_per_arena = arena_size / page_size;

        let arenas_lists = vec![null_mut::<Arena>(); max_pages_per_arena].into_boxed_slice();
        let old_arenas_lists = vec![null_mut::<Arena>(); max_pages_per_arena].into_boxed_slice();

        Self {
            current_arena: null_mut(),
            arena_size,

            page_size,
            num_uninitialized_pages: 0,
            old_arenas_lists,
            nblocks_for_size,
            max_pages_per_arena,
            sweep_color: CellState::White,
            min_empty_nfreepages: max_pages_per_arena,
            small_request_threshold,
            arenas_count: 0,
            footprint_limit,
            arenas_lists,
            tree: ArenaTree::new(),
            large_objects: vec![],
            bytes_allocated_this_cycle: 0,
            size_class_for_size_step: vec![],
            random: WeakRandom::new(None),
        }
    }

    pub fn flip_colors(&mut self) {
        if self.sweep_color == CellState::White {
            self.sweep_color = CellState::Black;
        } else {
            self.sweep_color = CellState::White;
        }
    }

    pub fn footprint_limit(&self) -> usize {
        self.footprint_limit
    }

    pub fn set_footprint_limit(&mut self, limit: usize) {
        self.footprint_limit = limit;
    }

    pub unsafe fn malloc(&mut self, size: usize) -> *mut u8 {
        if size > self.small_request_threshold {
            self.malloc_large(size)
        } else {
            self.malloc_small(align_usize(size, Self::WORD))
        }
    }

    #[inline]
    pub unsafe fn malloc_small(&mut self, size: usize) -> *mut u8 {
        /*let size_class = size >> Self::WORD_POWER_2;
        let size = size_class << Self::WORD_POWER_2;
        let mut page = *self.page_for_size.get_unchecked(size_class);
        if unlikely(page.is_null()) {
            page = self.allocate_new_page(size_class);
            if page.is_null() {
                return null_mut();
            }
        }

        let result = (*page).freelist;

        let freelist = (*result).next;

        (*page).freelist = freelist;

        if unlikely(freelist as usize - page as usize > self.page_size - size) {
            self.add_to_full(size_class, page);
        }
        (*(*page).arena).set_object_start(result as _);

        result.cast()*/
        todo!()
    }

    

    #[inline(never)]
    pub unsafe fn malloc_large(&mut self, size: usize) -> *mut u8 {
        if LargeObject::allocation_size(size) + self.bytes_allocated_this_cycle > self.footprint_limit {
            return null_mut();
        }
        let large = LargeObject::create(size);

        self.bytes_allocated_this_cycle += (*large).payload_size;
        self.large_objects.push((*large).payload_start());

        (*large).payload_start()
    }

    pub unsafe fn object_from_pointer(&self, maybe_middle_of_an_object: usize) -> *mut HeapObjectHeader {
        let lookup = self.tree.lookup(maybe_middle_of_an_object as _);

        if lookup.is_null() {
            return null_mut();
        }

        if (*lookup).typ == MemoryType::Large {
            let large = lookup.cast::<LargeObject>();
            return (*large).payload_start().cast();
        } else {
            let arena = lookup.cast::<Arena>();
            (*arena).find_object_start(maybe_middle_of_an_object as _).cast()
        }
    }

    unsafe fn pick_next_arena(&mut self) -> bool {
        for i in self.min_empty_nfreepages..self.max_pages_per_arena {
            if !self.arenas_lists[i].is_null() {
                self.current_arena = self.arenas_lists[i];
                self.arenas_lists[i] = (*self.current_arena).nextarena;
                return true;
            }
            self.min_empty_nfreepages = i + 1;
        }

        false
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn allocate_new_page(&mut self, size_class: usize) -> *mut Page {
        if self.current_arena.is_null() {
            self.allocate_new_arena();
            if self.current_arena.is_null() {
                return null_mut();
            }
        }

        let arena = self.current_arena;
        let result = (*arena).freepages as *mut *mut u8;

        let freepages = if (*arena).nfreepages > 0 {
            (*arena).nfreepages -= 1;
            result.read()
        } else {
            self.num_uninitialized_pages -= 1;
            if self.num_uninitialized_pages > 0 {
                result.cast::<u8>().add(self.page_size)
            } else {
                null_mut()
            }
        };

        (*arena).freepages = freepages as _;
        
        if freepages.is_null() {
            assert!((*arena).nfreepages == 0);
            (*arena).nextarena = self.arenas_lists[0];
            self.arenas_lists[0] = arena;
            self.current_arena = null_mut();
        }

        let page = result.cast::<Page>();
        page.write(Page {
            arena,
            nextpage: null_mut(),
        });
        //(*page).init_freelist(self.page_size, size_class << Self::WORD_POWER_2);

        page
    }

    pub unsafe fn allocate_new_arena(&mut self) {
        if self.pick_next_arena() {
            return;
        }

        self.rehash_arena_lists();

        if self.pick_next_arena() {
            return;
        }

        let arena = Arena::new(self.arena_size, self.page_size);
        self.num_uninitialized_pages = (*arena).npages as isize;
        self.current_arena = arena;
        self.arenas_count += 1;
        self.tree.add(arena.cast());
    }

    unsafe fn rehash_arena_lists(&mut self) {
        std::mem::swap(&mut self.old_arenas_lists, &mut self.arenas_lists);

        for i in 0..self.max_pages_per_arena {
            self.arenas_lists[i] = null_mut();
        }

        for i in 0..self.max_pages_per_arena {
            let mut arena = self.old_arenas_lists[i];

            while !arena.is_null() {
                let nextarena = (*arena).nextarena;

                if (*arena).nfreepages == (*arena).totalpages {
                    std::alloc::dealloc(
                        (*arena).base,
                        Layout::from_size_align_unchecked(self.arena_size, self.page_size),
                    );

                    let _ = Box::from_raw(arena);
                    self.arenas_count -= 1;
                    self.tree.remove(arena.cast());
                } else {
                    let n = (*arena).nfreepages;
                    (*arena).nextarena = self.arenas_lists[n];
                    self.arenas_lists[n] = arena;
                }

                arena = nextarena;
            }
        }
        self.min_empty_nfreepages = 1;
    }

    unsafe fn walk_page(
        &mut self,
        page: *mut Page,
        block_size: usize,
        ok_to_free_func: &mut dyn FnMut(*mut u8) -> bool,
    ) -> usize {
        
        //let surviving = (*page).sweep(self.page_size, block_size, ok_to_free_func);

        0
    }

    unsafe fn free_page(&mut self, page: *mut Page) {
        let arena = (*page).arena;
        (*arena).nfreepages += 1;
        core::ptr::write_bytes(page.cast::<u8>(), 0, self.page_size);
        page.cast::<*mut u8>().write((*arena).freepages as _);
        (*arena).freepages = page as _;
    }

   

    /// For each large object, if ok_to_free_func(obj) returns true, then free
    /// the object.
    ///
    /// # Note
    ///
    /// Must be invoked right after mark phase of GC, non incremental.
    pub unsafe fn mass_free_large(&mut self, ok_to_free_func: &mut dyn FnMut(*mut u8) -> bool) {
        let mut surviving = 0;
        self.large_objects.retain(|obj| {
            let large_obj = LargeObject::from_object(*obj);
            if ok_to_free_func(*obj) {
                Self::free_large_object(large_obj);
                false
            } else {
                surviving += (*large_obj).payload_size;
                true
            }
        });
        self.bytes_allocated_this_cycle += surviving;
    }

    unsafe fn free_large_object(obj: *mut LargeObject) {
        let sz = (*obj).payload_size;
        std::alloc::dealloc(obj.cast(), Layout::from_size_align_unchecked(sz, 32));
    }

    pub fn all_arenas(&self, mut f: impl FnMut(*mut Arena)) {
        if !self.current_arena.is_null() {
            f(self.current_arena);
        }

        for mut arena in self.arenas_lists.iter().copied() {
            unsafe {
                while !arena.is_null() {
                    f(arena);
                    arena = (*arena).nextarena;
                }
            }
        }
    }

    
}

/// A ArenaTree is a binary search tree of [Arena]'s sorted
/// by base addresses.
///
/// The tree does not keep its elements alive but merely provides
/// indexing capabilities.
pub struct ArenaTree {
    set: BTreeMap<usize, *mut BaseMemory>,
}

impl ArenaTree {
    pub fn new() -> Self {
        Self {
            set: Default::default(),
        }
    }

    pub fn add(&mut self, arena: *mut BaseMemory) {
        let base = unsafe { (*arena).payload() as usize };
        self.set.insert(base, arena);
    }

    pub fn remove(&mut self, arena: *mut BaseMemory) {
        let base = unsafe { (*arena).payload() as usize };
        self.set.remove(&base);
    }

    pub fn lookup(&self, maybe_points_to_middle_of_arena: *const u8) -> *mut BaseMemory {
        let base = maybe_points_to_middle_of_arena as usize;
        if let Some((_, arena)) = self.set.range(..=base).next_back() {
            *arena
        } else {
            null_mut()
        }
    }
}

pub fn out_of_memory(errmsg: &str) {
    std::panic::always_abort();
    panic!("{}", errmsg);
}

pub struct ObjectStartBitmap {
    offset: usize,
    bmp: Box<[u8]>,
}

impl ObjectStartBitmap {
    pub const fn bits_per_cell() -> usize {
        8 * size_of::<u8>()
    }

    pub const fn cell_mask() -> usize {
        Self::bits_per_cell() - 1
    }

    pub const fn bitmap_size(arena_size: usize) -> usize {
        (arena_size + ((Self::bits_per_cell() * ArenaCollection::WORD) - 1))
            / (Self::bits_per_cell() * ArenaCollection::WORD)
    }

    pub const fn reserved_for_bitmap(arena_size: usize) -> usize {
        (Self::bitmap_size(arena_size) + 15) & !15
    }

    pub fn new(offset: usize, arena_size: usize) -> Self {
        Self {
            offset,
            bmp: vec![0; Self::bitmap_size(arena_size)].into_boxed_slice(),
        }
    }

    pub const fn offset_index_bit(offset: usize) -> usize {
        (offset / 16) % Self::bits_per_cell()
    }

    fn cell(&self, ix: usize) -> u8 {
        unsafe {
            *self.bmp.get_unchecked(ix)
        }
    }

    pub fn find_object_start(&self, maybe_middle_of_an_object: usize) -> *mut u8 {
        let object_offset = maybe_middle_of_an_object.wrapping_sub(self.offset);
        let object_start_number = object_offset.wrapping_div(16);
        let mut cell_index = object_start_number.wrapping_div(Self::bits_per_cell());
        let bit = object_start_number & Self::cell_mask();
        let mut byte = self.cell(cell_index) & ((1 << (bit + 1)) - 1) as u8;

        while byte == 0 && cell_index > 0 {
            cell_index -= 1;
            byte = self.cell(cell_index);
        }

        if byte == 0 {
            return null_mut();
        }

        let object_start_number = (cell_index.wrapping_mul(Self::bits_per_cell()))
            .wrapping_add(Self::bits_per_cell() - 1)
            .wrapping_sub(byte.leading_zeros() as usize);

        let object_offset = object_start_number.wrapping_mul(16);
        let addr = object_offset.wrapping_add(self.offset);
        debug_assert!(addr >= self.offset);
        addr as _
    }

    #[inline]
    pub fn set_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        unsafe { *self.bmp.get_unchecked_mut(index) |= 1 << bit };
    }

    #[inline]
    pub fn clear_bit(&mut self, addr: usize) {
        let (index, bit) = self.object_start_index_bit(addr);
        self.bmp[index] &= !(1 << bit);
    }

    #[inline]
    pub fn check_bit(&self, addr: usize) -> bool {
        let (index, bit) = self.object_start_index_bit(addr);
        (self.bmp[index] & (1 << bit)) != 0
    }

    #[inline]
    pub fn object_start_index_bit(&self, addr: usize) -> (usize, usize) {
        let object_offset = addr.wrapping_sub(self.offset);
        let object_start_number = object_offset / ArenaCollection::WORD;

        (
            object_start_number / Self::bits_per_cell(),
            object_start_number & Self::cell_mask(),
        )
    }

    pub fn clear_range(&mut self, begin: usize, end: usize) {
        let mut begin_offset = begin.wrapping_sub(self.offset);
        let mut end_offset = end.wrapping_sub(self.offset);

        while begin_offset < end_offset && Self::offset_index_bit(begin_offset) != 0 {
            self.clear_bit(self.offset + begin_offset);
            begin_offset += ArenaCollection::WORD;
        }

        while begin_offset < end_offset && Self::offset_index_bit(end_offset) != 0 {
            end_offset -= ArenaCollection::WORD;
            self.clear_bit(self.offset + end_offset);
        }
    }

    pub fn clear(&mut self) {
        self.bmp.fill(0);
    }
}

#[repr(C)]
pub struct FreeListElement {
    tags: u64,
    next: *mut FreeListElement,
}

impl FreeListElement {
    pub unsafe fn as_element(addr: usize) -> *mut FreeListElement {
        let result = addr as *mut Self;
        let mut tags = 0;

        tags = VtableTag::update(0, tags);

        (*result).tags = tags;
        (*result).next = null_mut();
        result
    }
}

/// rounds the given value `val` up to the nearest multiple
/// of `align`.
pub fn align_usize(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }

    ((value + align - 1) / align) * align
}

#[inline]
pub const fn round_up(x: isize, n: isize) -> isize {
    round_down(x + n - 1, n)
}

#[inline]
pub const fn round_down(x: isize, n: isize) -> isize {
    x & -n
}

/// Allocator for single size-class 
pub struct LocalAllocator {
    next: *mut Self, 
    free_list: FreeList,
    current_page: *mut Page,
    full_pages: *mut Page,
    unswept_pages: *mut Page,
    size_class: usize,
}

impl LocalAllocator {
    unsafe fn did_consume_free_list(&mut self) {
        if !self.current_page.is_null() {
            (*self.current_page).nextpage = self.full_pages;
            self.full_pages = self.current_page;
        }
        self.current_page = null_mut();
        self.free_list.clear();
    }

    unsafe fn allocate_slowcase(&mut self, ac: &mut ArenaCollection) -> *mut u8 {
        ac.bytes_allocated_this_cycle += self.free_list.original_size;

        self.did_consume_free_list();

        if ac.bytes_allocated_this_cycle > ac.footprint_limit {
            // do GC and retry
            return null_mut();
        }
        
        let result = self.try_allocate_without_collecting(ac);

        if likely(!result.is_null()) {
            return result;
        }

        let page = ac.allocate_new_page(self.size_class);
        
        let result = self.try_allocate_in(page);
        result 
    }

    unsafe fn try_allocate_without_collecting(&mut self, ac: &mut ArenaCollection) -> *mut u8 {
        null_mut()
    }

    unsafe fn try_allocate_in(&mut self, page: *mut Page) -> *mut u8 {
        //(*page).sweep(&mut self.free_list);

        if self.free_list.allocation_will_fail() {
            (*page).nextpage = self.full_pages;
            self.full_pages = page;
            return null_mut();
        }

        self.current_page = page;

        let result = self.free_list.allocate(|| {
            unreachable!()
        });

        result
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct FreeCell {
    preserved_bits: u64,
    scramble_next: usize,
}

impl FreeCell {
    pub fn scramble(next: *mut Self, secret: usize) -> usize {
        (next as usize) ^ secret
    }

    pub fn descramble(scrambled: usize, secret: usize) -> *mut Self {
        (scrambled ^ secret) as *mut Self
    }

    pub fn set_next(&mut self, next: *mut Self, secret: usize) {
        self.scramble_next = Self::scramble(next, secret);
    }

    pub fn next(&self, secret: usize) -> *mut Self {
        Self::descramble(self.scramble_next, secret)
    }
}

pub struct FreeList {
    scrambled_head: usize,
    secret: usize,
    payload_end: *mut u8,
    remaining: usize,
    original_size: usize,
    cell_size: usize 
}

impl FreeList {
    pub fn head(&self) -> *mut FreeCell {
        FreeCell::descramble(self.scrambled_head, self.secret)
    }

    pub const fn cell_size(&self) -> usize {
        self.cell_size
    }

    pub fn allocation_will_fail(&self) -> bool {
        self.head().is_null() && self.remaining == 0
    }

    pub fn allocation_will_succeed(&self) -> bool {
        !self.allocation_will_fail()
    }

    pub fn allocate<F: FnMut() -> *mut u8>(&mut self, mut slow_path: F) -> *mut u8 {
        let mut remaining = self.remaining;

        if remaining != 0 {
            let cell_size = self.cell_size;
            remaining -= cell_size;
            self.remaining = remaining;
            return (self.payload_end as usize - remaining - cell_size) as _;
        }

        unsafe {
            let result = self.head();
            
            if unlikely(result.is_null()) {
                return slow_path();
            }

            self.scrambled_head = (*result).scramble_next;
            result as _
        }
    }

    pub fn clear(&mut self) {
        self.scrambled_head = 0;
        self.remaining = 0;
        self.secret = 0;
        self.payload_end = null_mut();
        self.original_size = 0;
    }

    pub fn initialize_list(&mut self, head: *mut FreeCell, secret: usize, bytes: usize) {
        self.scrambled_head = FreeCell::scramble(head, secret);
        self.secret = secret;
        self.payload_end = null_mut();
        self.remaining = 0;
        self.original_size = bytes; 
    }

    pub fn initialize_bump(&mut self, payload_end: *mut u8, remaining: usize) {
        self.scrambled_head = 0;
        self.secret = 0;
        self.payload_end = payload_end;
        self.remaining = remaining;
        self.original_size = remaining;
    }

}