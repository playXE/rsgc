const MULTIPLY_DE_BRUIJN_BIT_POSITION: [isize; 32] = [
    0, 9,  1,  10, 13, 21, 2,  29, 11, 14, 16, 18, 22, 25, 3, 30,
    8, 12, 20, 28, 15, 17, 24, 7,  19, 27, 23, 6,  26, 5,  4, 31
];

pub fn log2floor(v: usize) -> isize {
    let mut v = v as isize;
    v |= v >> 1; // first round down to one less than a power of 2
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v |= v >> 16;
   
    MULTIPLY_DE_BRUIJN_BIT_POSITION[((v as u32).wrapping_mul(0x07C4ACDD) >> 27) as usize]
}

pub fn log2ceil(v: usize) -> isize {
    log2floor(2 * v - 1)
}

pub const fn round_to_next_multiple(value: usize, multiple: usize) -> usize {
    assert!(multiple.is_power_of_two());
    (value + multiple - 1) & !(multiple - 1)
}

pub const fn div_and_round_up(value: usize, divisor: usize) -> usize {
    (value + divisor - 1) / divisor
}