#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum BlockState {
    EmptyCommited,
    EmptyUncommited,
    Unmarked,
    Marked,
    Reusable
}

pub struct Block {
    index: usize,
    state: BlockState,
    begin: *mut u8,
    end: *mut u8,
}

impl Block {
    pub fn is_empty(&self) -> bool {
        self.state == BlockState::EmptyCommited || self.state == BlockState::EmptyUncommited
    }

    pub fn is_alloc_allowed(&self) -> bool {
        self.state == BlockState::Reusable || self.is_empty()
    }

    pub fn index(&self) -> usize {
        self.index
    }


}