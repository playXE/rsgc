use super::object_header::*;

pub trait Visitor {
    unsafe fn visit_pointers_len(&mut self, first: *const *mut ObjectHeader, len: usize) {
        let mut start = first;
        while start < first.add(len) {
            self.visit_pointer(start.read());
            start = start.add(1);
        }
    }

    unsafe fn visit_pointer(&mut self, object: *mut ObjectHeader);

    unsafe fn visit_conservative(&mut self, from: *mut u8, to: *mut u8);

    fn visit_weak_persistent_handles(&self) -> bool {
        false
    }
}

pub trait ObjectVisitor {
    fn visit_object(&mut self, ptr: *mut ObjectHeader);
}

pub trait FindObjectVisitor {
    fn filter_addr(&self) -> usize {
        0
    }

    fn visit_range(&self, begin_addr: usize, end_addr: usize) -> bool {
        let addr = self.filter_addr();
        (addr == 0) || (begin_addr <= addr) && (addr < end_addr)
    }

    fn find_object(&self, ptr: *mut ObjectHeader) -> bool;
}


pub struct VisitCounter<'a> {
    count: usize,
    pub visitor: &'a mut dyn Visitor,
}

impl<'a> VisitCounter<'a> {
    pub fn new(visitor: &'a mut dyn Visitor) -> Self {
        Self { count: 0, visitor }
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

impl<'a> Visitor for VisitCounter<'a> {
    unsafe fn visit_pointer(&mut self, object: *mut ObjectHeader) {
        self.count += 1;
        self.visitor.visit_pointer(object);
    }

    unsafe fn visit_conservative(&mut self, from: *mut u8, to: *mut u8) {
        self.visitor.visit_conservative(from, to);
    }
}