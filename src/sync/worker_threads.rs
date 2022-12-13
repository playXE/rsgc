use core::num;
use std::{
    cell::{Cell, UnsafeCell},
    sync::{
        atomic::Ordering,
        atomic::{AtomicBool, AtomicUsize},
        Arc,
    }, ptr::null,
};

use parking_lot::{Condvar, Mutex};
use super::semaphore::Sem;

pub trait WorkerTask: Send + Sync {
    fn name(&self) -> &str;

    fn work(&self, worker_id: usize);
}

impl WorkerTask for () {
    fn name(&self) -> &str {
        "no-op"
    }
    fn work(&self, worker_id: usize) {
        let _ = worker_id;
    }
}

pub struct WorkerTaskDispatcher {
    task: UnsafeCell<*const dyn WorkerTask>,
    started: AtomicUsize,
    not_finished: AtomicUsize,
    start_semaphore: Sem,
    end_semaphore: Sem,
    shut_down: AtomicBool,
    did_shutdown: AtomicUsize,
    mutex: Mutex<bool>,
    cv: Condvar,
}

impl WorkerTaskDispatcher {

    fn new() -> Self {
        Self {
            task: UnsafeCell::new(null::<()>()),
            started: AtomicUsize::new(0),
            not_finished: AtomicUsize::new(0),
            start_semaphore: Sem::new(0).unwrap(),
            end_semaphore: Sem::new(0).unwrap(),
            shut_down: AtomicBool::new(false),
            did_shutdown: AtomicUsize::new(0),
            mutex: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    fn coordinator_distribute_task<'a>(&self, task: &'a dyn WorkerTask, num_workers: usize) {
        // Safety: We are the only thread that can write to the task pointer
        // AND we wait for workers to finish before returning, so no memory leaks are possible,
        // and `task` would not outlive its owner.
        unsafe {
            self.task.get().write(std::mem::transmute(task));
        }

        self.not_finished.store(num_workers, Ordering::Relaxed);
        self.start_semaphore.signal(num_workers);
        self.end_semaphore.wait();

        unsafe {
            let p = &mut *self.task.get();
            *p = null::<()>();
        }
    }

    fn worker_run_task(&self) {
        // Wait for the coordinator to dispatch a task.
        self.start_semaphore.wait();

        // Get and set worker id.
        let worker_id = self.started.fetch_add(1, Ordering::AcqRel);
        WorkerThread::set_worker_id(worker_id);

        // Run task.
        unsafe {
            let task_ptr = self.task.get();
            if task_ptr.read().is_null() {
                return;
            }
            let task_ref = &**task_ptr;
            task_ref.work(worker_id);
        }
        // Mark that the worker is done with the task.
        // The worker is not allowed to read the state variables after this line.
        let not_finished = self.not_finished.fetch_sub(1, Ordering::AcqRel);
        // The last worker signals to the coordinator that all work is completed.
        if not_finished == 1 {
            self.end_semaphore.signal(1);
        }
    }
}

pub struct WorkerThreads {
    name: &'static str,
    workers: Vec<Arc<WorkerThread>>,
    max_workers: usize,
    created_workers: usize,
    active_workers: usize,
    dispatcher: Arc<WorkerTaskDispatcher>,
}

impl WorkerThreads {
    pub fn active_workers(&self) -> usize {
        self.active_workers
    }
    pub fn new(name: &'static str, max_workers: usize) -> Self {
        Self {
            name,
            workers: Vec::with_capacity(max_workers),
            created_workers: 0,
            active_workers: 0,
            dispatcher: Arc::new(WorkerTaskDispatcher::new()),
            max_workers
        }
    }

    pub fn initialize_workers(&mut self) {
        self.set_active_workers(self.max_workers);
    }

    fn create_worker(&mut self) {
        let worker = Arc::new(WorkerThread {
            dispatcher: self.dispatcher.clone(),
        });

        self.workers.push(worker.clone());

        let _ = std::thread::spawn(move || {
            worker.run();
        });
        
        let mut started = self.dispatcher.mutex.lock();
        if !*started {
            self.dispatcher.cv.wait(&mut started);
        }

        *started = false;
    }

    pub fn set_active_workers(&mut self, num_workers: usize) -> usize {
        while self.created_workers < num_workers {
            self.create_worker();
            self.created_workers += 1;
        }

        self.active_workers = self.created_workers.min(num_workers);
        self.active_workers
    }

    /// Executes `task` in parallel on all workers and waits for them to finish.
    pub fn run_task<'a>(&self, task: &'a dyn WorkerTask) {
        self.dispatcher.coordinator_distribute_task(task, self.active_workers);
    }

    pub fn run_task_wworkers<'a>(&mut self, task: &'a dyn WorkerTask, num_workers: usize) {
        let x = self.active_workers;
        self.set_active_workers(num_workers);
        self.dispatcher.coordinator_distribute_task(task, num_workers);
        self.set_active_workers(x);
    }
}

thread_local! {
    static WORKER_ID: Cell<usize> = Cell::new(usize::MAX);
}

pub struct WorkerThread {
    dispatcher: Arc<WorkerTaskDispatcher>,
}
unsafe impl Send for WorkerThread {}
unsafe impl Sync for WorkerThread {}
impl WorkerThread {
    pub fn worker_id() -> usize {
        WORKER_ID.with(|id| id.get())
    }

    fn set_worker_id(id_: usize) {
        WORKER_ID.with(|id| id.set(id_));
    }

    fn run(&self) {
        {
            let mut started = self.dispatcher.mutex.lock();
            *started = true;
            self.dispatcher.cv.notify_one();
        }
        
        while !self.dispatcher.shut_down.load(Ordering::Relaxed) {
            self.dispatcher.worker_run_task();
        }
        self.dispatcher.did_shutdown.fetch_sub(1, Ordering::AcqRel);
    }
}
