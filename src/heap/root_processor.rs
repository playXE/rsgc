use crate::heap::taskqueue::MarkTask;
use crate::{object::HeapObjectHeader, utils::stack::Stack, traits::Visitor};
use crate::utils::taskqueue::*;
use super::{heap::{Heap, heap}, thread::ThreadInfo};

/// RootsCollector
///
/// Synchronously collects all root objects and pushes them to the marking context queues.
///
/// # TODO
///
/// Replace this with parallel/concurrent root scanner
pub struct RootsCollector<'a> {
    heap: &'a mut Heap,
    mark_stack: Stack<*mut HeapObjectHeader>,
}

impl<'a> RootsCollector<'a> {
    pub fn new(heap: &'a mut Heap) -> Self {
        Self {
            heap,
            mark_stack: Stack::new(Stack::<*mut HeapObjectHeader>::DEFAULT_SEGMENT_SIZE, 4, 0),
        }
    }

    pub fn try_mark(&mut self, obj: *const HeapObjectHeader) {
        if self.heap.marking_context().mark(obj) {
            self.mark_stack.push(obj as _);
        }
    }

    pub fn try_mark_conservative(&mut self, ptr: *const u8) {
        let obj = self.heap.object_start(ptr as _);

        if !obj.is_null() {
            self.try_mark(obj);
        }
    }

    fn mark_stack(&mut self, start: *mut u8, end: *mut u8) {
        let mut cursor = start.cast::<*const u8>();
        let mut end = end.cast::<*const u8>();

        // in case stack grows in different direction
        if cursor > end {
            std::mem::swap(&mut cursor, &mut end); 
        }

        while cursor < end {
            unsafe {
                let potential_object = cursor.read();
                self.try_mark_conservative(potential_object);
                cursor = cursor.add(1);
            }
        }
    }

    pub fn collect(&mut self, threads: &'a [*mut ThreadInfo]) {
        unsafe {
            for thread in threads.iter().copied() {
                self.mark_stack((*thread).stack_start(), (*thread).last_sp());
            }

            heap().walk_global_roots(self);

        }
    }

    pub fn get_mark_stack(self) -> Stack<*mut HeapObjectHeader> {
        self.mark_stack
    }
}

impl<'a> Visitor for RootsCollector<'a> {
    fn visit(&mut self, object: *const u8) {
        let hdr = unsafe { object.cast::<HeapObjectHeader>().sub(1) };

        self.try_mark(hdr);
    }
    fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize) {
        for i in 0..len {
            let ptr = unsafe { *ptrs.add(i) };
            self.try_mark_conservative(ptr);
        }
    }
}

