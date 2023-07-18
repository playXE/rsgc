
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
#[allow(non_camel_case_types)]
pub struct u24([u8; 3]);

impl From<u32> for u24 {
    fn from(value: u32) -> Self {
        let b0 = (value & 0xFF) as u8;
        let b1 = ((value >> 8) & 0xFF) as u8;
        let b2 = ((value >> 16) & 0xFF) as u8;
        u24([b0, b1, b2])
    }
}

impl Into<u32> for u24 {
    fn into(self) -> u32 {
        let b0 = self.0[0] as u32;
        let b1 = (self.0[1] as u32) << 8;
        let b2 = (self.0[2] as u32) << 16;
        b0 | b1 | b2
    }
}

impl std::ops::Add<i32> for u24 {
    type Output = Self;

    fn add(self, rhs: i32) -> Self::Output {
        let x: u32 = self.into();

        u24::from((x as i32 + rhs) as u32)
    }
}
