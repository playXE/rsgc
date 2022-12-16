use std::mem::size_of;

pub mod stack;
pub mod taskqueue;
pub mod free_list_allocator;
pub mod number_seq;
pub mod ptr_queue;
pub mod deque;

pub const fn nth_bit(n: usize) -> usize {
    if n >= size_of::<usize>() * 8 {
        0
    } else {
        1 << n 
    }
}

pub const fn right_nth_bit(n: usize) -> usize {
    nth_bit(n) - 1
}