pub mod heap;
pub mod thread;
pub mod safepoint;
pub mod bitmap;
pub mod concurrent_thread;
pub mod controller;
pub mod free_list;
pub mod free_set;
pub mod region;
pub mod mark_sweep;
pub mod memory_region;
pub mod marking;
pub mod virtual_memory;
pub mod sweeper;
pub mod stack;
pub mod shared_vars;
pub mod signals;
pub mod tlab;
pub mod immix;


#[inline(always)]
pub const fn align_down(addr: usize, align: usize) -> usize {
    addr & !align.wrapping_sub(1)
}
#[inline(always)]
pub const fn align_up(addr: usize, align: usize) -> usize {
    // See https://github.com/rust-lang/rust/blob/e620d0f337d0643c757bab791fc7d88d63217704/src/libcore/alloc.rs#L192
    addr.wrapping_sub(align).wrapping_sub(1) & !align.wrapping_sub(1)
}
#[inline(always)]
pub const fn is_aligned(addr: usize, align: usize) -> bool {
    addr & align.wrapping_sub(1) == 0
}



/// rounds the given value `val` up to the nearest multiple
/// of `align`.
#[inline(always)]
pub const fn align_usize(value: usize, align: usize) -> usize {
    ((value.wrapping_add(align).wrapping_sub(1)).wrapping_div(align)).wrapping_mul(align)
    //((value + align - 1) / align) * align
}

#[inline]
pub fn which_power_of_two(value: usize) -> usize {
    value.trailing_zeros() as _
}

pub fn round_up_to_power_of_two32(mut value: u32) -> u32 {
    if value > 0 {
        value -= 1;
    }
    1 << (32 - value.leading_zeros())
}
#[inline]
pub fn round_down_to_power_of_two32(value: u32) -> u32 {
    if value > 0x80000000 {
        return 0x80000000;
    }

    let mut result = round_up_to_power_of_two32(value);
    if result > value {
        result >>= 1;
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum AllocType {
    ForLAB,
    Shared, 
}

pub struct AllocRequest {
    min_size: usize,
    requested_size: usize,
    actual_size: usize,
    typ: AllocType
}

impl AllocRequest {
    pub const fn new(typ: AllocType, min_size: usize, requested_size: usize) -> Self {
        Self {
            typ,
            min_size,
            requested_size,
            actual_size: 0,
        }
    }

    pub fn for_lab(&self) -> bool {
        self.typ == AllocType::ForLAB
    }

    pub const fn size(&self) -> usize {
        self.requested_size
    }

    pub const fn min_size(&self) -> usize {
        self.min_size
    }

    pub const fn actual_size(&self) -> usize {
        self.actual_size
    }

    pub fn set_actual_size(&mut self, actual_size: usize) {
        self.actual_size = actual_size;
    }
}

#[derive(Debug)]
pub struct DynBitmap {
    buffer: Vec<u8>,
    bit_count: usize,
}

impl DynBitmap {
    pub fn contained(bits: usize) -> Self {
        Self {
            buffer: vec![0u8; Self::bytes_required(bits)],
            bit_count: bits,
        }
    }
    pub const fn bytes_required(bits: usize) -> usize {
        (bits + 7) / 8
    }

    /// Index of contained bit byte.
    fn contained_byte_index(bit_index: usize) -> usize {
        bit_index / 8
    }

    /// Bit position in byte.
    const fn position_in_byte(bit_index: usize) -> u8 {
        (bit_index % 8) as u8
    }

    fn get_byte(&self, bit_index: usize) -> u8 {
        unsafe {  self.buffer
            .get(Self::contained_byte_index(bit_index))
            .copied()
            .unwrap_unchecked() }
    }

    fn get_byte_mut(&mut self, bit_index: usize) -> &mut u8 {
        unsafe { self.buffer
            .get_mut(Self::contained_byte_index(bit_index))
            .unwrap_unchecked() }
    }

    /// Get `value` from `byte` for exact bit-`index`.
    #[inline(always)]
    fn get_value(byte: u8, index: u8) -> bool {
        // We shift byte-value on `index`-bit and apply bit **and**-operation
        // with `0b0000_0001`.
        ((byte >> index) & 0b0000_0001) == 1
    }

    pub fn get(&self, bit_index: usize) -> bool {
        let byte: u8 = self.get_byte(bit_index);
        let position_in_byte = Self::position_in_byte(bit_index);
        Self::get_value(byte, position_in_byte)
    }

    /// Set `value` in `byte` for exact bit-`index`.
    #[inline(always)]
    fn set_value(byte: u8, value: bool, index: u8) -> u8 {
        // Unset `index` bit and set value.
        byte & !(1 << index) | ((value as u8) << index)
    }


    pub fn set(&mut self, bit_index: usize, value: bool)  {
        let byte: &mut u8 = self.get_byte_mut(bit_index);
        let position: u8 = Self::position_in_byte(bit_index);
        *byte = Self::set_value(*byte, value, position);
    }


    pub fn write<W: std::io::Write>(&self, mut writer: W) -> std::io::Result<()> {
        writer.write_all(&self.buffer)
    }

    
    pub fn byte_size(&self) -> usize {
        self.buffer.len()
    }

    pub fn arity(&self) -> usize {
        self.bit_count
    }


    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        self.buffer
            .iter()
            .flat_map(|&byte| (0..=7).map(move |idx| Self::get_value(byte, idx)))
            .take(self.arity())
    }

    pub fn clear(&mut self) {
        self.buffer.fill(0);
    }

    pub fn count_ones(&self) -> usize {
        self.buffer.iter().map(|x| x.count_ones() as usize).sum()
    }
}

impl std::iter::FromIterator<bool> for DynBitmap {
    fn from_iter<I: IntoIterator<Item = bool>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let initial_size = iter.size_hint().1.map(Self::bytes_required).unwrap_or(0);

        let mut buffer = Vec::with_capacity(initial_size);
        let mut bit_idx: u8 = 0;
        let mut byte: u8 = 0;
        let mut bit_count = 0;

        for value in iter {
            if bit_idx == 8 {
                buffer.push(byte);
                byte = 0;
                bit_idx = 0;
            }

            byte = DynBitmap::set_value(byte, value, bit_idx);
            bit_idx += 1;
            bit_count += 1;
        }

        buffer.push(byte);

        Self { buffer, bit_count }
    }
}


