#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FreeLineMeta {
    pub next: i8,
    pub size: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LineFlag {
    Empty = 0x0,
    Marked = 0x1,
}

