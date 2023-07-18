use crate::constants::{WORDS_IN_BLOCK, ALLOCATION_ALIGNMENT_WORDS};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectMeta {
    Free = 0x0,
    Placeholder = 0x1,
    Allocated = 0x2,
    Marked = 0x4,
}

pub unsafe fn object_meta_is_free(metadata: *const ObjectMeta) -> bool {
    metadata.read() == ObjectMeta::Free
}

pub unsafe fn object_meta_is_placeholder(metadata: *const ObjectMeta) -> bool {
    metadata.read() == ObjectMeta::Placeholder
}

pub unsafe fn object_meta_is_allocated(metadata: *const ObjectMeta) -> bool {
    metadata.read() == ObjectMeta::Allocated
}

pub unsafe fn object_meta_is_marked(metadata: *const ObjectMeta) -> bool {
    metadata.read() == ObjectMeta::Marked
}

pub unsafe fn clear_line_at(cursor: *mut ObjectMeta) {
    core::ptr::write_bytes(
        cursor.cast::<u8>(),
        0,
        WORDS_IN_BLOCK / ALLOCATION_ALIGNMENT_WORDS
    )
}

pub unsafe fn clear_block_at(cursor: *mut ObjectMeta) {
    core::ptr::write_bytes(
        cursor.cast::<u8>(),
        0,
        WORDS_IN_BLOCK / ALLOCATION_ALIGNMENT_WORDS
    )
}

const SWEEP_MASK: u64 = 0x0404040404040404;

pub unsafe fn sweep_line_at(start: *mut ObjectMeta) {
    //    implements this, just with hardcoded constants:
    //
    //    size_t startIndex = Bytemap_index(bytemap, start);
    //    size_t endIndex = startIndex + WORDS_IN_LINE /
    //    ALLOCATION_ALIGNMENT_WORDS; ObjectMeta *data = bytemap->data;
    //
    //    for (size_t i = startIndex; i < endIndex; i++) {
    //        if (data[i] == om_marked) {
    //            data[i] = om_allocated;
    //        } else {
    //            data[i] = om_free;
    //        }
    //    }

    let first = start.cast::<u64>();
    first.write((first.read() as u64 & SWEEP_MASK) >> 1);
    first.add(1).write((first.add(1).read() as u64 & SWEEP_MASK) >> 1);
}

pub unsafe fn sweep(cursor: *mut ObjectMeta) {
    cursor.cast::<u8>().write((cursor.read() as u8 & 0x04) >> 1);
}