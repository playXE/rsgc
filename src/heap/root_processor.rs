use std::mem::size_of;
use std::sync::Arc;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};

use super::mark::Terminator;
use super::{align_down, align_up};
use super::{
    heap::{heap, Heap},
    thread::Thread,
};
use crate::heap::taskqueue::MarkTask;
use crate::utils::taskqueue::*;
use crate::{object::HeapObjectHeader, traits::Visitor, utils::stack::Stack};

pub struct RootSet {
    roots: Vec<Arc<dyn Root>>,
}

impl RootSet {
    pub fn new() -> Self {
        Self { roots: Vec::new() }
    }

    pub fn add_root(&mut self, root: Arc<dyn Root>) {
        self.roots.push(root);
    }

    pub fn add_roots(&mut self, roots: &[Arc<dyn Root>]) {
        self.roots.extend_from_slice(roots);
    }

    pub fn roots(&self) -> &[Arc<dyn Root>] {
        &self.roots
    }

    pub fn process(&self) {
        let heap = heap();
        let processor = RootProcessor::new(if heap.options().parallel_root_mark_tasks {
            heap.workers().workers()
        } else {
            1
        });

        for root in self.roots.iter() {
            processor.add_root(root.clone());
        }

        if heap.options().parallel_root_mark_tasks {
            // spawn workers, all roots will go to parallel mark worksets.
            heap.workers().scoped(|scope| {
                for i in 0..processor.workers.len() {
                    let task_id = i;
                    let processor = &processor;
                    scope.execute(move || {
                        let mut processor = ThreadRootProcessor::new(task_id, &processor, unsafe {
                            &mut *crate::heap::heap::heap()
                                .marking_context()
                                .mark_queues()
                                .visitor(i)
                        });

                        processor.run();
                    });
                }
            });
        } else {
            let mut processor = ThreadRootProcessor::new(
                0,
                &processor,
                &mut *crate::heap::heap::heap().marking_context_mut(), // all roots will go to injector queue
            );

            processor.run();
        }
    }
}

pub trait Root: Send + Sync {
    fn name(&self) -> &str;
    fn abbreviated_name(&self) -> &str;

    fn execute(&self, processor: &mut ThreadRootProcessor);
}

pub struct RootTask {
    root: Arc<dyn Root>,
    task: Box<dyn FnMut(&mut ThreadRootProcessor)>,
}

pub struct RootProcessor {
    injector: Injector<RootTask>,
    workers: Vec<Worker<RootTask>>,
    stealers: Vec<Stealer<RootTask>>,
    terminator: Terminator,
}

unsafe impl Send for RootProcessor {}
unsafe impl Sync for RootProcessor {}

impl RootProcessor {
    pub fn new(nworkers: usize) -> Self {
        let workers = (0..nworkers)
            .map(|_| Worker::new_lifo())
            .collect::<Vec<_>>();

        let stealers = workers.iter().map(|w| w.stealer()).collect();

        Self {
            injector: Injector::new(),
            workers,
            stealers,
            terminator: Terminator::new(nworkers),
        }
    }

    pub fn add_root(&self, root: Arc<dyn Root>) {
        let task = RootTask {
            root: root.clone(),
            task: Box::new(move |processor| root.execute(processor)),
        };
        self.injector.push(task);
    }

    pub fn add_roots(&self, roots: &[Arc<dyn Root>]) {
        for root in roots {
            self.add_root(root.clone());
        }
    }

    pub fn add_worker(&mut self, worker: Worker<RootTask>) {
        self.workers.push(worker);
    }
}

pub struct ThreadRootProcessor<'a> {
    task_id: usize,
    processor: &'a RootProcessor,
    visitor: &'a mut dyn Visitor,
    current_root: Option<Arc<dyn Root>>,
}

impl<'a> ThreadRootProcessor<'a> {
    pub fn task_id(&self) -> usize {
        self.task_id
    }

    fn new(task_id: usize, processor: &'a RootProcessor, visitor: &'a mut dyn Visitor) -> Self {
        Self {
            processor,
            visitor,
            task_id,
            current_root: None
        }
    }

    fn worker(&self) -> &Worker<RootTask> {
        &self.processor.workers[self.task_id]
    }

    fn pick_next(&self) -> Option<RootTask> {
        self.worker().pop().or_else(|| loop {
            match self.processor.injector.steal_batch_and_pop(self.worker()) {
                Steal::Retry => continue,
                Steal::Success(x) => break Some(x),
                Steal::Empty => break None,
            }
        })
    }

    pub fn add_task(&self, task: impl FnMut(&mut ThreadRootProcessor) + 'static, to_local: bool) {
        let task = RootTask { root: self.current_root.as_ref().map(|x| x.clone()).unwrap(), task: Box::new(task) };
        if to_local {
            self.worker().push(task);
        } else {
            self.processor.injector.push(task);
        }
    }

    pub fn visitor(&mut self) -> &mut dyn Visitor {
        self.visitor
    }

    fn run(&mut self) {
        loop {
            match self.pick_next() {
                Some(mut task) => {
                    self.current_root = Some(task.root.clone());
                    (task.task)(self);
                    self.current_root = None;
                }
                None => {
                    if self.processor.terminator.try_terminate() {
                        break;
                    }
                }
            }
        }
    }
}

unsafe impl Send for ThreadRootProcessor<'_> {}
unsafe impl Sync for ThreadRootProcessor<'_> {}


pub struct SimpleRoot {
    name: &'static str,
    abbreviated_name: &'static str,
    callback: Box<dyn Fn(&mut ThreadRootProcessor) + Send + Sync>,
}

impl Root for SimpleRoot {
    fn name(&self) -> &str {
        self.name
    }

    fn abbreviated_name(&self) -> &str {
        self.abbreviated_name
    }

    fn execute(&self, processor: &mut ThreadRootProcessor) {
        (self.callback)(processor);
    }
}

impl SimpleRoot {
    pub fn new(
        name: &'static str,
        abbreviated_name: &'static str,
        callback: impl Fn(&mut ThreadRootProcessor) + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            abbreviated_name,
            callback: Box::new(callback),
        }
    }
}