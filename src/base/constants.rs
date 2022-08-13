use std::mem::size_of;

use crate::memory::object_header::ObjectHeader;
pub struct ObjectAlignment<const WORD_SIZE: usize, const WORD_SIZE_LOG2: usize>;

impl<const WORD_SIZE: usize, const WORD_SIZE_LOG2: usize>
    ObjectAlignment<WORD_SIZE, WORD_SIZE_LOG2>
{
    pub const NEW_OBJECT_ALIGNMENT_OFFSET: usize = WORD_SIZE;
    pub const OLD_OBJECT_ALIGNMENT_OFFSET: usize = 0;
    pub const NEW_OBJECT_BIT_POSITION: usize = WORD_SIZE_LOG2;

    pub const OBJECT_ALIGNMENT: usize = 2 * WORD_SIZE;
    pub const OBJECT_ALIGNMENT_LOG2: usize = WORD_SIZE_LOG2 + 1;
    pub const OBJECT_ALIGNMENT_MASK: usize = Self::OBJECT_ALIGNMENT - 1;

    pub const BOOL_VALUE_BIT_POSITION: usize = Self::OBJECT_ALIGNMENT_LOG2;
    pub const BOOL_VALUE_MASK: usize = 1 << Self::BOOL_VALUE_BIT_POSITION;

    pub const BOOL_VS_NULL_BIT_POSITION: usize = Self::OBJECT_ALIGNMENT_LOG2 + 1;
    pub const BOOL_VS_NULL_MASK: usize = 1 << Self::BOOL_VS_NULL_BIT_POSITION;

    pub const TRUE_OFFSET_FROM_NULL: usize = Self::OBJECT_ALIGNMENT * 2;
    pub const FALSE_OFFSET_FROM_NULL: usize = Self::OBJECT_ALIGNMENT * 3;
}

pub const I32_SIZE_LOG: usize = 2;
pub const I64_SIZE_LOG: usize = 3;

pub type HostObjectAlignment = ObjectAlignment<
    { size_of::<usize>() },
    {
        #[cfg(target_pointer_width = "32")]
        {
            I32_SIZE_LOG
        }
        #[cfg(target_pointer_width = "64")]
        {
            I64_SIZE_LOG
        }
    },
>;

pub const OBJECT_ALIGNMENT: usize = HostObjectAlignment::OBJECT_ALIGNMENT;
pub const OBJECT_ALIGNMENT_LOG2: usize = HostObjectAlignment::OBJECT_ALIGNMENT_LOG2;
pub const OBJECT_ALIGNMENT_MASK: usize = HostObjectAlignment::OBJECT_ALIGNMENT_MASK;
pub const NEW_OBJECT_ALIGNMENT_OFFSET: usize = HostObjectAlignment::NEW_OBJECT_ALIGNMENT_OFFSET;
pub const OLD_OBJECT_ALIGNMENT_OFFSET: usize = HostObjectAlignment::OLD_OBJECT_ALIGNMENT_OFFSET;
pub const NEW_OBJECT_BIT_POSITION: usize = HostObjectAlignment::NEW_OBJECT_BIT_POSITION;
pub const BOOL_VALUE_BIT_POSITION: usize = HostObjectAlignment::BOOL_VALUE_BIT_POSITION;
pub const BOOL_VALUE_MASK: usize = HostObjectAlignment::BOOL_VALUE_MASK;
pub const BOOL_VS_NULL_BIT_POSITION: usize = HostObjectAlignment::BOOL_VS_NULL_BIT_POSITION;
pub const BOOL_VS_NULL_MASK: usize = HostObjectAlignment::BOOL_VS_NULL_MASK;
pub const TRUE_OFFSET_FROM_NULL: usize = HostObjectAlignment::TRUE_OFFSET_FROM_NULL;
pub const FALSE_OFFSET_FROM_NULL: usize = HostObjectAlignment::FALSE_OFFSET_FROM_NULL;

pub const MAX_OBJECT_ALIGNMENT: usize = 16;

#[cfg(target_pointer_width = "64")]
pub const WORD_SIZE_LOG2: usize = 3;
#[cfg(target_pointer_width = "32")]
pub const WORD_SIZE_LOG2: usize = 2;

pub const BITS_PER_BYTE_LOG2: usize = 3;

pub const BITS_PER_WORD: usize = 1 << BITS_PER_WORD_LOG2;
pub const BITS_PER_WORD_LOG2: usize = WORD_SIZE_LOG2 + BITS_PER_BYTE_LOG2;

#[cfg(target_pointer_width = "64")]
pub const ALLOCATION_GRANULARITY: usize = 16;
#[cfg(target_pointer_width = "32")]
pub const ALLOCATION_GRANULARITY: usize = 8;

pub const ALLOCATION_MASK: usize = ALLOCATION_GRANULARITY - 1;
pub const PADDING_SIZE: usize = ALLOCATION_GRANULARITY - size_of::<ObjectHeader>();

cfg_if::cfg_if! {
    if #[cfg(all(target_arch="aarch64", target_os="macos"))] {
        pub const GUARD_PAGE_SIZE: usize = 0;
    }
    else {
        pub const GUARD_PAGE_SIZE: usize 4096;
    }
}

pub const FREE_LIST_ENTRY_SIZE: usize = 2 * size_of::<usize>();
