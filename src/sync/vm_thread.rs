use std::{
    ptr::{null, null_mut, DynMetadata},
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize},
    time::Duration,
};

use atomic::Ordering;

use crate::heap::{safepoint, thread::Thread};

use super::monitor::Monitor;

pub trait VMOperation {
    fn doit(&mut self, threads: &[*mut Thread]);

    fn doit_prologue(&mut self, threads: &[*mut Thread]) -> bool {
        let _ = threads;
        true
    }

    fn doit_epilogue(&mut self, threads: &[*mut Thread]) {
        let _ = threads;
    }

    fn name(&self) -> &'static str;

    /// An operation can either be done inside a safepoint
    /// or concurrently with Rust threads running.
    fn evaluate_at_safepoint(&self) -> bool {
        true
    }

    fn allow_nested_vm_operations(&self) -> bool {
        false
    }
}

impl VMOperation for () {
    fn doit(&mut self, threads: &[*mut Thread]) {
        let _ = threads;
    }

    fn name(&self) -> &'static str {
        "no-op"
    }
}

#[derive(Clone, Copy)]
struct VMOperationRef {
    meta: DynMetadata<dyn VMOperation>,
    ptr: *mut (),
}

impl VMOperationRef {
    pub const fn null() -> Self {
        let meta = core::ptr::metadata(core::ptr::null::<()>() as *const dyn VMOperation);
        Self {
            meta,
            ptr: null_mut(),
        }
    }

    pub unsafe fn as_dyn(&mut self) -> &mut dyn VMOperation {
        &mut *core::ptr::from_raw_parts_mut(self.ptr, self.meta)
    }
}

static mut CUR_VM_OPERATION: VMOperationRef = VMOperationRef::null();
static mut NEXT_VM_OPERATION: VMOperationRef = VMOperationRef::null();
static VM_OPERATION_LOCK: Monitor<()> = Monitor::new(());
static NOTIFY_LOCK: Monitor<()> = Monitor::new(());
static IS_RUNNING: AtomicBool = AtomicBool::new(false);
static VM_THREAD: AtomicPtr<VMThread> = AtomicPtr::new(null_mut());
static SHOULD_TERMINATE: AtomicBool = AtomicBool::new(false);
static TERMINATED: AtomicBool = AtomicBool::new(false);
static TERMINATE_LOCK: Monitor<()> = Monitor::new(());
static GUARANTEED_SAFEPOINT_INTERVAL: AtomicUsize = AtomicUsize::new(1000);
///
/// A single VMThread is used by other threads to offload heavy vm operations
// like scavenge, garbage_collect etc.
///
pub struct VMThread {}

impl VMThread {
    pub fn is_running() -> bool {
        IS_RUNNING.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub fn create() {
        assert!(
            VM_THREAD.load(Ordering::Relaxed).is_null(),
            "we can only allocate one VMThread"
        );

        VM_THREAD.store(Box::into_raw(Box::new(VMThread {})), Ordering::Relaxed);
    }

    pub fn should_terminate() -> bool {
        SHOULD_TERMINATE.load(Ordering::Relaxed)
    }

    pub fn is_terminated() -> bool {
        TERMINATED.load(Ordering::Relaxed)
    }

    pub(crate) fn run() {}

    unsafe fn wait_for_operation() {
        let mut ml_op_lock = VM_OPERATION_LOCK.lock(false);

        NEXT_VM_OPERATION = VMOperationRef::null();

        ml_op_lock.notify_all();

        while !Self::should_terminate() {
            if !NEXT_VM_OPERATION.ptr.is_null() {
                return;
            }

            ml_op_lock.notify_all();
            if GUARANTEED_SAFEPOINT_INTERVAL.load(Ordering::Relaxed) > 0 {
                ml_op_lock.wait_for(Duration::from_millis(
                    GUARANTEED_SAFEPOINT_INTERVAL.load(Ordering::Relaxed) as u64,
                ));
            } else {
                ml_op_lock.wait();
            }
        }
    }

    unsafe fn inner_execute(mut op: VMOperationRef) {
        let mut prev_vm_operation = VMOperationRef::null();

        if !CUR_VM_OPERATION.ptr.is_null() {
            if !CUR_VM_OPERATION.as_dyn().allow_nested_vm_operations() {
                eprintln!("Unexpected nested VM operation {} requested by operation {}", op.as_dyn().name(), CUR_VM_OPERATION.as_dyn().name());
                std::process::abort();
            }
            prev_vm_operation = CUR_VM_OPERATION;
        }

        CUR_VM_OPERATION = op;
    }

    pub(crate) unsafe fn loop_() {
        safepoint::init();

        loop {
            if Self::should_terminate() {
                break;
            }
            Self::wait_for_operation();
            if Self::should_terminate() {
                break;
            }
        }
    }
}
