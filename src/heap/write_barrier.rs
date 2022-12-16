//! Write barrier infrastructure. 
//! 
//! Since RSGC is concurrent write barriers are required to properly scan all objects. 
//! 
//! # Usage 
//! 
//! Write barrier should be inserted right before GC ref gets written to an object:
//! ```mustfail,rust
//! 
//! fn set_x(obj: Handle<Obj>, x: Handle<X>) {
//!    thread().write_barrier(obj);
//!    obj.as_mut().x = x;
//! }
//! 
//! ```
//! 
//! # Implementation
//! 
//! RSGC uses black mutator + Yuasa deletion barrier. 
//! 
//! The downside of this method is amount of floating garbage that does not get collected,
//! but it offers fast cycle termination at the cost of higher memory pressure.

use crate::{object::Handle, traits::Object};

use super::thread::{ThreadInfo, thread};

pub trait WriteBarriered {
    fn write_barrier_handle(&self) -> Handle<dyn Object>;
}

impl<T: Object + ?Sized> WriteBarriered for Handle<T> {
    fn write_barrier_handle(&self) -> Handle<dyn Object> {
        Handle::as_dyn(self)
    }
}


/// Type that automatically inserts write-barrier when mutable reference is taken.
pub struct WriteBarrier<T: Object + ?Sized> {
    handle: Handle<T>
}

impl<T: Object + ?Sized> WriteBarrier<T> {
    pub fn new(handle: Handle<T>) -> Self {
        Self {
            handle
        }
    }

    pub fn handle(&self) -> Handle<T> {
        self.handle
    }

    /// Returns mutable reference to `T` and automatically inserts write barrier.
    pub fn as_mut_fast(&mut self, thread: &mut ThreadInfo) -> &mut T 
    where T: Sized
    {
        thread.write_barrier(self.handle);
        self.handle.as_mut()
    }

    /// Returns mutable reference to `T` and automatically inserts write barrier.
    /// 
    /// ## Note
    /// 
    /// Might be slower because requires TLS access, use [WriteBarrier::as_mut_fast] instead 
    /// if you have `ThreadInfo` pointer in context.
    pub fn as_mut(&mut self) -> &mut T 
    where T: Sized
    {
        self.as_mut_fast(thread())
    }

    pub fn as_ref(&self) -> &T where T: Sized {
        self.handle.as_ref()
    }

    pub fn as_dyn(&self) -> Handle<dyn Object> {
        Handle::as_dyn(&self.handle)
    }
}

impl<T: Object + ?Sized> Copy for WriteBarrier<T> {}
impl<T: Object + ?Sized> Clone for WriteBarrier<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Object + ?Sized> WriteBarriered for WriteBarrier<T> {
    fn write_barrier_handle(&self) -> Handle<dyn Object> {
        Handle::as_dyn(&self.handle)
    }
}

