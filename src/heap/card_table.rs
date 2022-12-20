//! Simple two-level card table implementation.

use std::{mem::size_of, sync::atomic::AtomicUsize, intrinsics::likely};

use crate::utils::{is_aligned, sloppy_memset};

use super::{virtual_memory::{VirtualMemory, page_size}, bitmap::HeapBitmap, heap::heap, align_usize};

unsafe fn byte_cas(old_value: u8, new_value: u8, mut addr: *mut u8) -> bool {
    let shift_in_bytes = addr as usize % size_of::<usize>();
    addr = addr.sub(shift_in_bytes);

    let shift_in_bits = shift_in_bytes * size_of::<u8>() * 8;

    let word_atomic = addr.cast::<AtomicUsize>();

    let cur_word = (*word_atomic).load(atomic::Ordering::Relaxed) & !(0xff << shift_in_bits);

    let old_word = cur_word | (old_value as usize) << shift_in_bits;
    let new_word = cur_word | (new_value as usize) << shift_in_bits;

    (*word_atomic).compare_exchange_weak(old_word, new_word, atomic::Ordering::Relaxed, atomic::Ordering::Relaxed).is_ok()
}



pub struct CardTable {
    mem_map: VirtualMemory,
    biased_begin: usize,
    offset: isize,
}

impl CardTable {
    pub const CARD_SHIFT: usize = 10;
    pub const CARD_SIZE: usize = 1 << Self::CARD_SHIFT;
    pub const CARD_CLEAN: u8 = 0;
    pub const CARD_DIRTY: u8 = 0x70;
    pub const CARD_AGED: u8 = Self::CARD_DIRTY - 1;

    pub fn new(heap_begin: *mut u8, heap_capacity: usize) -> Self {
        let capacity = heap_capacity / Self::CARD_SIZE;
        let size = align_usize(capacity + 256, page_size());
        let mem_map = VirtualMemory::allocate_aligned(size, page_size(), false, "card table").unwrap();

        let cardtable_begin = mem_map.address();

        let mut offset = 0;

        let mut biased_begin = (cardtable_begin as usize - (heap_begin as usize >> Self::CARD_SHIFT)) as *mut u8;

        let biased_byte = biased_begin as usize & 0xff;

        if biased_byte != Self::CARD_DIRTY as usize {
            let delta = Self::CARD_DIRTY as isize - biased_byte as isize;
            offset = delta + if delta < 0 {
                0x100
            } else {
                0
            };

            biased_begin = unsafe { biased_begin.offset(offset) };
        }

        Self {
            biased_begin: biased_begin as _,
            offset,
            mem_map
        }
    }

    /// Returns a value that when added to a heap address >> CARD_SHIFT will address the appropriate
    /// card table byte. For convenience this value is cached in every Thread.
    pub fn get_biased_begin(&self) -> usize {
        self.biased_begin
    }

    pub fn mem_map_begin(&self) -> usize {
        self.mem_map.start()
    }

    pub fn mem_map_end(&self) -> usize {
        self.mem_map.end()
    }

    pub fn card_from_addr(&self, addr: usize) -> *mut u8 {
        let card_addr = self.biased_begin + (addr >> Self::CARD_SHIFT);

        card_addr as *mut u8
    }

    pub fn is_valid_card(&self, card: *mut u8) -> bool {
        let card_addr = card as usize;

        card_addr >= self.mem_map_begin() && card_addr < self.mem_map_end()
    }

    pub fn addr_from_card(&self, card: *mut u8) -> usize {
        let card_addr = card as usize;

        assert!(self.is_valid_card(card));

        (card_addr - self.biased_begin) << Self::CARD_SHIFT
    }

    pub fn is_dirty(&self, card: *mut u8) -> bool {
        unsafe { *card == Self::CARD_DIRTY as u8 }
    }
    
    pub fn is_clean(&self, card: *mut u8) -> bool {
        unsafe { *card == Self::CARD_CLEAN as u8 }
    }

    pub fn get_card(&self, addr: usize) -> u8 {
        unsafe { *self.card_from_addr(addr) }
    }

    pub fn mark_card(&self, addr: usize) {
        unsafe { *self.card_from_addr(addr) = Self::CARD_DIRTY as u8 }
    }

    pub fn visit_clear(&mut self, start: usize, end: usize, mut visitor: impl FnMut(*mut u8)) {
        let mut card_start = self.card_from_addr(start);
        let card_end = self.card_from_addr(end);

        while card_start != card_end {
            unsafe {
                if card_start.read() == Self::CARD_DIRTY {
                    card_start.write(Self::CARD_CLEAN);
                    visitor(card_start);
                }
                card_start = card_start.add(1);
            }
        }
    }

    pub fn scan<const CLEAR_CARD: bool>(
        &self,
        bitmap: &HeapBitmap<16>,
        scan_begin: *mut u8,
        scan_end: *mut u8,
        visitor: impl Fn(*mut u8),
        minimum_age: u8 
    ) -> usize {
        let card_begin = self.card_from_addr(scan_begin as _);
        let card_end = self.card_from_addr(scan_end as _);
        let mut card_cur = card_begin;
        let vis = |ptr| {
            visitor(ptr as *mut u8)
        };
        let mut cards_scanned = 0;

        while !is_aligned(card_cur as usize, size_of::<usize>(), 0) && card_cur < card_end {
            unsafe {
                if card_cur.read() >= minimum_age {
                    let start = self.addr_from_card(card_cur);
                    bitmap.visit_marked_range(start, start + Self::CARD_SIZE, vis);
                    cards_scanned += 1;
                }
                card_cur = card_cur.add(1);
            }

        }

        if card_cur < card_end {
            let aligned_end = (card_end as usize - (card_end as usize & (size_of::<usize>() - 1))) as *mut u8;

            let word_end = aligned_end as *mut usize;
            let mut word_cur = card_cur.cast();

            'exit: while word_cur < word_end {
                unsafe {
                    while word_cur.read() == 0 {
                        word_cur = word_cur.add(1);
                        if word_cur >= word_end {
                            break 'exit;
                        }
                    }

                    let mut start_word = word_cur.read();
                    let mut start = self.addr_from_card(word_cur.cast()) as usize;

                    for i in 0..size_of::<usize>() {
                        if start_word >= minimum_age as usize {
                            let card = word_cur.cast::<u8>().add(i);
                            debug_assert!(card.read() == start_word as u8 || *card == Self::CARD_DIRTY);
                            bitmap.visit_marked_range(start, start + Self::CARD_SIZE, vis);
                            cards_scanned += 1;
                        }
                        start_word >>= 8;
                        start += Self::CARD_SIZE;
                    }

                    

                    word_cur = word_cur.add(1);
                }
            }

            card_cur = word_end.cast::<u8>();

            while card_cur < card_end {
                unsafe {
                    if card_cur.read() >= minimum_age {
                        let start = self.addr_from_card(card_cur);
                        bitmap.visit_marked_range(start, start + Self::CARD_SIZE, vis);
                        cards_scanned += 1;
                    }

                    card_cur = card_cur.add(1);
                }
            }
        }

        if CLEAR_CARD {
            self.clear_card_range(scan_begin, scan_end);
        }
        cards_scanned
    }

    pub fn modify_cards_atomic(&self, scan_begin: *mut u8, scan_end: *mut u8, visitor: impl Fn(u8) -> u8, modified: impl Fn(*mut u8, u8, u8)) {
        unsafe {
            let mut card_cur = self.card_from_addr(scan_begin as _);
            let mut card_end = self.card_from_addr(scan_end as _);


            while !is_aligned(card_cur as usize, size_of::<usize>(), 0) && card_cur < card_end {
                let (mut expected, mut new_value);

                loop {
                    expected = card_cur.read();
                    new_value = visitor(expected);
                    if expected == new_value && byte_cas(expected, new_value, card_cur) {
                        break;
                    }
                }

                if expected != new_value {
                    modified(card_cur, expected, new_value);
                }

                card_cur = card_cur.add(1);
            }

            while !is_aligned(card_cur as usize, size_of::<usize>(), 0) && card_end > card_cur {
                card_end = card_end.sub(1);
                let (mut expected, mut new_value);

                loop {
                    expected = card_cur.read();
                    new_value = visitor(expected);
                    if expected == new_value && byte_cas(expected, new_value, card_end) {
                        break;
                    }
                }

                if expected != new_value {
                    modified(card_end, expected, new_value);
                }
            }

            let mut word_cur = card_cur.cast::<usize>();
            let word_end = card_end.cast::<usize>();

            union U1 {
                expected_word: usize,
                expected_bytes: [u8; size_of::<usize>()]
            }

            union U2 {
                new_word: usize,
                new_bytes: [u8; size_of::<usize>()]
            }

            let mut u1 = U1 {
                expected_word: 0
            };

            let mut u2 = U2 {
                new_word: 0
            };

            while word_cur < word_end {
                loop {
                    u1.expected_word = word_cur.read();

                    if likely(u1.expected_word == 0) /* All CARD_CLEAN */ {
                        break;
                    }

                    for i in 0..size_of::<usize>() {
                        u2.new_bytes[i] = visitor(u1.expected_bytes[i]);
                    }

                    let atomic_word = word_cur.cast::<AtomicUsize>();

                    if likely((*atomic_word).compare_exchange_weak(u1.expected_word, u2.new_word, atomic::Ordering::Relaxed, atomic::Ordering::Relaxed).is_ok()) {
                        for i in 0..size_of::<usize>() {
                            let expected_byte = u1.expected_bytes[i];
                            let new_byte = u2.new_bytes[i];

                            if expected_byte != new_byte {
                                modified(word_cur.cast::<u8>().add(i), expected_byte, new_byte);
                            }
                        }

                        break;
                    }
                }
                word_cur = word_cur.add(1);
            }
        }
    }

    pub fn clear_card_range(&self, start: *mut u8, end: *mut u8) {
        let card_start = self.card_from_addr(start as _);
        let card_end = self.card_from_addr(end as _);

        unsafe {
            sloppy_memset::sloppy_memset(card_start, Self::CARD_CLEAN, card_end as usize - card_start as usize);
        }
    }
}

pub fn age_card_visitor(card: u8) -> u8 {
    if card == CardTable::CARD_DIRTY {
        card - 1 
    } else {
        0
    }
}