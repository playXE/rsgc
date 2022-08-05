pub const fn round_up(x: isize, n: isize) -> isize {
    round_down(x + n - 1, n)
}
pub const fn round_down(x: isize, n: isize) -> isize {
    x & -n
}

const fn next_bits(x: usize) -> usize {
    if x == 1 {
        1
    } else {
        x / 2
    }
}

pub const fn count_leading_zeros_16_(bits: usize, value: u16) -> u16 {
    if bits == 1 {
        return value ^ 1;
    }

    let upper_half = value >> (bits as u16 / 2);
    let next_value = if upper_half != 0 { upper_half } else { value };

    let add = if upper_half != 0 { 0 } else { bits as u16 / 2 };

    count_leading_zeros_16_(next_bits(bits), next_value) + add
}

pub const fn count_leading_zeros_8_(bits: usize, value: u8) -> u8 {
    if bits == 1 {
        return value ^ 1;
    }

    let upper_half = value >> (bits as u8 / 2);
    let next_value = if upper_half != 0 { upper_half } else { value };

    let add = if upper_half != 0 { 0 } else { bits as u8 / 2 };

    count_leading_zeros_8_(next_bits(bits), next_value) + add
}

pub const fn count_leading_zeros_32_(bits: usize, value: u32) -> u32 {
    if bits == 1 {
        return value ^ 1;
    }

    let upper_half = value >> (bits as u32 / 2);
    let next_value = if upper_half != 0 { upper_half } else { value };

    let add = if upper_half != 0 { 0 } else { bits as u32 / 2 };

    count_leading_zeros_32_(next_bits(bits), next_value) + add
}

pub const fn count_leading_zeros_64_(bits: usize, value: u64) -> u64 {
    if bits == 1 {
        return value ^ 1;
    }

    let upper_half = value >> (bits as u64 / 2);
    let next_value = if upper_half != 0 { upper_half } else { value };

    let add = if upper_half != 0 { 0 } else { bits as u64 / 2 };

    count_leading_zeros_64_(next_bits(bits), next_value) + add
}

pub const fn count_leading_zeros_32(value: u32) -> u32 {
    count_leading_zeros_32_(32, value)
}

pub const fn count_leading_zeros_64(value: u64) -> u64 {
    count_leading_zeros_64_(64, value)
}

pub const fn count_leading_zeros_16(value: u16) -> u16 {
    count_leading_zeros_16_(16, value)
}

pub const fn count_leading_zeros_8(value: u8) -> u8 {
    count_leading_zeros_8_(8, value)
}

pub const fn count_leading_zeros(value: usize) -> usize {
    #[cfg(target_pointer_width = "64")]
    {
        count_leading_zeros_64(value as u64) as usize
    }
    #[cfg(target_pointer_width = "32")]
    {
        count_leading_zeros_32(value as u32) as usize
    }
}

fn read_float_and_factor_from_env(var: &str) -> Option<(f64, usize)> {
    let value = std::env::var(var);

    match value {
        Ok(mut value) => {
            if value.len() > 0 {
                if value.len() > 1
                    && (value.as_bytes()[value.len() - 1] == 'b' as u8
                        || value.as_bytes()[value.len() - 1] == 'B' as u8)
                {
                    value = value.as_str()[0..value.len() - 1].to_string();
                }
                let mut realvalue = value.as_str()[0..value.len() - 1].to_string();

                let at = value.len() - 1;
                let factor;
                if value.as_bytes()[at] == 'g' as u8 || value.as_bytes()[at] == 'G' as u8 {
                    factor = 1024 * 1024 * 1024;
                } else if value.as_bytes()[at] == 'm' as u8 || value.as_bytes()[at] == 'M' as u8 {
                    factor = 1024 * 1024;
                } else if value.as_bytes()[at] == 'k' as u8 || value.as_bytes()[at] == 'K' as u8 {
                    factor = 1024;
                } else {
                    realvalue = value;
                    factor = 1;
                }

                match realvalue.parse::<f64>() {
                    Ok(x) => Some((x, factor)),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn read_uint_from_env(var: &str) -> Option<usize> {
    let (value, factor) = read_float_and_factor_from_env(var)?;

    Some(value as usize * factor)
}

pub fn read_float_from_env(var: &str) -> Option<f64> {
    read_float_and_factor_from_env(var).map(|x| x.0)
}

pub fn read_string_from_env(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

#[derive(Copy, Clone)]
pub struct TinyBloomFilter {
    bits: usize,
}
impl TinyBloomFilter {
    pub const fn new(bits: usize) -> Self {
        Self { bits }
    }

    pub fn rule_out(&self, bits: usize) -> bool {
        if bits == 0 {
            return true;
        }
        if (bits & self.bits) != bits {
            return true;
        }
        false
    }

    pub fn add(&mut self, other: &Self) {
        self.bits |= other.bits;
    }

    pub fn add_bits(&mut self, bits: usize) {
        self.bits |= bits;
    }

    pub fn reset(&mut self) {
        self.bits = 0;
    }

    pub fn bits(&self) -> usize {
        self.bits
    }
}