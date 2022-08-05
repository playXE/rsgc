use std::mem::size_of;

use super::constants::*;

pub struct BitSet<const N: isize>
where
    [(); 1 + ((N as usize - 1) / (size_of::<isize>() * 8))]: Copy,
{
    data: [isize; 1 + ((N as usize - 1) / (size_of::<isize>() * 8))],
}

impl<const N: isize> BitSet<N>
where
    [(); 1 + ((N as usize - 1) / (size_of::<isize>() * 8))]: Copy,
{
    pub const LENGTH_IN_WORDS: usize = 1 + ((N as usize - 1) / (size_of::<isize>() * 8));

    pub fn reset(&mut self) {
        self.data.fill(0);
    }

    pub fn size(&self) -> isize {
        N
    }

    pub fn clear_last_and_find_previous(&mut self, current_last: isize) -> isize {
        let mut w = current_last >> BITS_PER_WORD_LOG2;
        let mut bits = self.data[w as usize];
        bits ^= 1 << (current_last & (BITS_PER_WORD as isize - 1));
        self.data[w as usize] = bits;
        while bits == 0 && w > 0 {
            w -= 1;
            bits = self.data[w as usize];
        }

        if bits == 0 {
            return -1;
        } else {
            ((w + 1) << BITS_PER_WORD_LOG2) - bits.leading_zeros() as isize - 1
        }
    }

    pub fn last(&self) -> isize {
        let mut w = Self::LENGTH_IN_WORDS as isize - 1;
        while w >= 0 {
            let d = self.data[w as usize];
            if d != 0 {
                return ((w as isize + 1) << BITS_PER_WORD_LOG2) - d.leading_zeros() as isize - 1;
            }
            w -= 1;
        }

        -1
    }

    pub fn next(&mut self, i: isize) -> isize {
        let mut w = i >> BITS_PER_WORD_LOG2;

        let mask = !0 << (i & (BITS_PER_WORD as isize - 1));

        if (self.data[w as usize] & mask) != 0 {
            let tz = (self.data[w as usize] & mask).trailing_zeros();
            return (w << BITS_PER_WORD_LOG2) + tz as isize;
        }

        loop {
            w += 1;
            if w < Self::LENGTH_IN_WORDS as isize {
                if self.data[w as usize] != 0 {
                    return (w << BITS_PER_WORD_LOG2)
                        + self.data[w as usize].trailing_zeros() as isize;
                }
            } else {
                break;
            }
        }
        -1
    }

    pub fn test(&self, i: isize) -> bool {
        let w = i >> BITS_PER_WORD_LOG2;
        let mask = 1 << (i & (BITS_PER_WORD as isize - 1));
        (self.data[w as usize] & mask) != 0
    }

    pub fn set(&mut self, i: isize, value: bool) {
        let w = i >> BITS_PER_WORD_LOG2;
        let mask = 1 << (i & (BITS_PER_WORD as isize - 1));
        if value {
            self.data[w as usize] |= mask;
        } else {
            self.data[w as usize] &= !mask;
        }
    }

    pub fn new() -> Self {
        Self {
            data: [0; 1 + ((N as usize - 1) / (size_of::<isize>() * 8))],
        }
    }
}
