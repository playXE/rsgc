pub trait Object {
    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }

    fn finalize(&mut self) {
        unsafe {
            core::ptr::drop_in_place(self);
        }
    }

    fn process_weak(&mut self, processor: &mut dyn WeakProcessor) {
        let _ = processor;
    }
}

pub trait Visitor {
    fn visit(&mut self, object: *const u8);
    fn visit_conservative(&mut self, ptrs: *const *const u8, len: usize);
}

pub trait WeakProcessor {
    fn process(&mut self, object: *const u8) -> *const u8;
}