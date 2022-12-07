use crate::object::HeapObjectHeader;

pub struct MarkingContext {
    mark_stack: Vec<*mut HeapObjectHeader>,
}

impl MarkingContext {
    pub unsafe fn is_marked(&self, p: *mut HeapObjectHeader) -> bool {
        (*p).is_marked()
    }

    pub unsafe fn unmark(&self, p: *mut HeapObjectHeader) {
        (*p).clear_marked()
    }

    pub unsafe fn mark(&self, p: *mut HeapObjectHeader) {
        (*p).set_marked()
    }
}