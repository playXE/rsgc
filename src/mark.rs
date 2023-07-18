use std::mem::size_of;

use crate::{
    heap::Heap,
    meta::object::ObjectMeta,
    object_header::ObjectHeader,
    thread::Thread,
    traits::Visitor,
    utils::{bytemap::Bytemap, mcontext::PlatformRegisters},
};

pub struct Marker {
    heap: *mut Heap,
    bytemap: *mut Bytemap,
    stack: Vec<*mut ObjectHeader>,
}

impl Visitor for Marker {
    unsafe fn visit(&mut self, object: *const u8) {
        unsafe {
            if (*self.heap).is_word_in_heap(object.cast()) {
                let meta = (*self.bytemap).get(object.cast());

                if meta.read() == ObjectMeta::Allocated {
                    self.mark_object(object as _, meta);
                }
            }
        }
    }

    unsafe fn visit_range(&mut self, _: *const u8, _: *const u8) {}

    unsafe fn visit_range_conservatively(&mut self, start: *const u8, end: *const u8) {
        let mut cursor = start.cast::<*const usize>();
        let end = end.cast::<*const usize>();
        while cursor <= end {
            let ptr = cursor.read();
            if !ptr.is_null() {
               
                if (*self.heap).is_word_in_heap(ptr) {
                    self.mark_conservative(cursor.read());
                }
            }

            cursor = cursor.add(1);
        }
    }
}

impl Marker {
    pub(crate) fn new(heap: &mut Heap) -> Self {
        Self {
            bytemap: heap.bytemap,
            heap,
            stack: Vec::with_capacity(128),
        }
    }

    unsafe fn mark_object(&mut self, object: *mut ObjectHeader, meta: *mut ObjectMeta) {
        ObjectHeader::mark(&mut *self.heap, object, meta);
        self.stack.push(object);
    }

    unsafe fn mark_conservative(&mut self, address: *const usize) {
        let object = ObjectHeader::get_unmarked_object(&mut *self.heap, address as _);
        if !object.is_null() {
            let meta = (*self.bytemap).get(object.cast());

            if meta.read() == ObjectMeta::Allocated {
               
                self.mark_object(object, meta);
            }
        }
    }

    unsafe fn mark_program_stack(&mut self, thread: &mut Thread) {
        let stack_bottom = thread.stack.origin;
        let mut stack_top = thread.stack_top.load(std::sync::atomic::Ordering::SeqCst);

        loop {
            if !stack_top.is_null() {
                break;
            }

            stack_top = thread.stack_top.load(std::sync::atomic::Ordering::SeqCst);
        }

        let mut start = stack_bottom;
        let mut end = stack_top.cast::<u8>();
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
      
        self.visit_range_conservatively(start.cast(), end.cast());

        let regs = thread.execution_context.as_ptr().cast::<u8>();
        let regs_end = regs.add(size_of::<PlatformRegisters>());
       
        self.visit_range_conservatively(regs, regs_end);
    }

    pub(crate) unsafe fn mark(&mut self) {
        while let Some(object) = self.stack.pop() {
            let rtti = (*object).rtti;
        
            (rtti.trace)(object.add(1).cast(), self);
        }
    }

    pub(crate) unsafe fn mark_roots(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);

        let mut head = (*self.heap).threads;

        while !head.is_null() {
            self.mark_program_stack(&mut *(*head).thread);
            head = (*head).next;
        }

        self.mark();
    }
}
