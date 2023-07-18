#![feature(thread_local, core_intrinsics)]

use object_header::Handle;
use thread::Thread;
/// Current type of execution by given threadin foreign scope be included in the
/// stop-the-world mechanism, as they're assumed to not modify the state of the
/// GC. Upon conversion from Managed to Unmanged state calling thread shall dump
/// the contents of the register to the stack and save the top address of the
/// stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    /// Thread executes code using GC following cooperative mode -
    /// it periodically polls for synchronization events.
    Managed,
    /// Thread executes foreign code (syscalls, C functions) and is not
    /// able to modify state of the GC. Upon synchronization event garbage collector
    /// would ignore this thread. Upon returning from foreign execution thread
    /// would stop until synchronization event would finish.
    Unmanaged,
}

pub(crate) mod safepoint;
pub(crate) mod memory_map;
pub(crate) mod constants;
pub mod utils;
pub mod object_header;
pub mod meta;
pub mod traits;
pub mod gc_roots;
pub mod heap;
pub mod block_allocator;
pub mod thread;
pub mod large_allocator;
pub mod allocator;
pub mod stack;
pub mod mark;

pub fn new_i32(thread: &mut Thread, x: i32) -> Handle<i32> {
    thread.allocate(x)
}