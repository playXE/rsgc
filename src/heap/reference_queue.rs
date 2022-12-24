use crossbeam_deque::{Injector, Steal, Worker};

use crate::system::object::HeapObjectHeader;

pub struct ReferenceQueue {
    workers: Vec<Worker<Reference>>,
    injector: Injector<Reference>,
}

impl ReferenceQueue {
    pub fn new() -> Self {
        Self {
            workers: Vec::new(),
            injector: Injector::new(),
        }
    }

    pub fn add(&self, worker_id: usize, reference: Reference) {
        self.workers[worker_id].push(reference);
    }

    pub fn pop(&self, worker_id: usize) -> Option<Reference> {
        loop {
            match self.injector.steal_batch_and_pop(&self.workers[worker_id]) {
                Steal::Success(reference) => return Some(reference),
                Steal::Empty => break,
                Steal::Retry => continue,
            }
        }

        None
    }
}

pub struct Reference {
    
    referent: *mut *mut HeapObjectHeader,
}

unsafe impl Send for Reference {}

pub struct ReferenceProcessor {
    weak_reference_queue: ReferenceQueue,
}
