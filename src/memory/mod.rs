use std::{
    any::TypeId,
    hash::Hash,
    marker::PhantomData,
    mem::{MaybeUninit, size_of},
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use crate::base::formatted_size;

use self::{
    object_header::{ObjectHeader, VTable},
    page::{PageSpaceConfig, Pages},
    traits::{Allocation, Finalize, ManagedObject, Trace},
};

//pub mod free_list_old;
pub mod free_list;
pub mod marker;
pub mod object_header;
pub mod object_start_bitmap;
pub mod page;
pub mod page_memory;
pub mod pointer_block;
pub mod traits;
pub mod visitor;

pub struct Heap {
    pages: Pages,
}

impl Heap {
    pub fn new(config: PageSpaceConfig) -> Self {
        Self {
            pages: Pages::new(config),
        }
    }

    pub fn add_persistent_root<T: 'static + Trace>(&mut self, root: T) -> u32 {
        let object = Box::new(root);

        self.pages.trace_callbacks.insert(self.pages.key, object);
        self.pages.key += 1;

        self.pages.key - 1
    }

    pub fn remove_persistent_root(&mut self, key: u32) -> Option<Box<dyn Trace>> {
        self.pages.trace_callbacks.remove(&key)
    }

    pub fn uninit<T: 'static + Allocation + ManagedObject>(&mut self) -> Managed<MaybeUninit<T>> {
        let header = self.pages.malloc_fixedsize::<T>();
        unsafe {
            Managed {
                ptr: NonNull::new_unchecked(header.add(1).cast()),
                marker: PhantomData,
            }
        }
    }

    pub unsafe fn malloc(
        &mut self,
        vtable: &'static VTable,
        size: usize,
        has_gcptrs: bool,
        light_finalizer: bool,
        finalize: bool,
        has_weakptr: bool,
    ) -> *mut ObjectHeader {
        self.pages.malloc_manual(
            vtable,
            size,
            has_gcptrs,
            light_finalizer,
            finalize,
            has_weakptr,
        )
    }

    pub fn manage<T: 'static + Allocation + ManagedObject>(&mut self, value: T) -> Managed<T> {
        let header = self.pages.malloc_fixedsize::<T>();
        unsafe {
            (*header).data_mut().cast::<T>().write(value);
            //(*header).set_initialized();
            Managed {
                ptr: NonNull::new_unchecked(header.add(1).cast()),
                marker: PhantomData,
            }
        }
    }
    /// Allocates memory for variable sized object and returns uninitialized object. Note that
    /// length field is actually initialized with `length` that is passed there.
    pub fn varsize<T: 'static + Allocation + ManagedObject>(
        &mut self,
        length: usize,
    ) -> Managed<MaybeUninit<T>> {
        let header = self.pages.malloc_varsize::<T>(length);
        unsafe {
            Managed {
                ptr: NonNull::new_unchecked(header.add(1).cast()),
                marker: PhantomData,
            }
        }
    }
    /// Creates a new [`WeakRef<T>`](WeakRef) to a [`Managed<T>`](Managed) object.
    pub fn weak<T: 'static + ManagedObject + ?Sized>(&mut self, value: Managed<T>) -> WeakRef<T> {
        let object = self.manage(WeakRefInner {
            field: value.ptr() as *mut u8,
            marker: Default::default(),
        });

        WeakRef { pointer: object }
    }

    pub unsafe fn add_weak_map<T: Allocation + ManagedObject + ?Sized>(&mut self, mut obj: Managed<T>) {
        assert!(T::__IS_WEAKMAP, "T::__IS_WEAKMAP must be set to true");
        self.pages.weak_maps.push(obj.header_mut() as *mut ObjectHeader);
    }

    #[inline(never)]
    #[cold]
    pub fn collect(&mut self) {
        self.pages.collect();
    }

    pub fn statistics(&self) -> HeapStats {
        unsafe {
            let (npages, nsize) = self.pages.normal_pages_statistics();
            let (lpages, lsize) = self.pages.large_pages_statistics();
            let (spages, ssize) = self.pages.sweep_pages_statistics();
            HeapStats {
                used_bytes: self.pages.controller().used_bytes,
                next_collection_initial: self.pages.controller().next_collection_initial,
                next_collection_threshold: self.pages.controller().next_collection_threshold,
                gc_count: self.pages.controller().gc_count,
                normal_pages_count: npages,
                normal_pages_size: nsize,
                large_pages_count: lpages,
                large_pages_size: lsize,
                sweep_pages_count: spages,
                sweep_pages_size: ssize,
                weak_ref_count: self.pages.weak_refs.len(),
                weak_map_count: self.pages.weak_maps.len(),
                finalizable_count: self.pages.finalizable.len(),
                destructible_count: self.pages.destructible.len(),
                queued_finalizers: self.pages.run_finalizers.len(),
            }
        }
    }
}

#[derive(Debug)]
pub struct HeapStats {
    pub used_bytes: usize,
    pub next_collection_initial: usize,
    pub next_collection_threshold: usize,
    pub gc_count: usize,
    pub normal_pages_count: usize,
    pub normal_pages_size: usize,
    pub large_pages_count: usize,
    pub large_pages_size: usize,
    pub sweep_pages_count: usize,
    pub sweep_pages_size: usize,
    pub weak_ref_count: usize,
    pub weak_map_count: usize,
    pub destructible_count: usize,
    pub finalizable_count: usize,
    pub queued_finalizers: usize,
}

impl std::fmt::Display for HeapStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "heap statistics:")?;
        writeln!(f, "  used: {}", formatted_size(self.used_bytes))?;
        writeln!(
            f,
            "  next collection initial: {}",
            formatted_size(self.next_collection_initial)
        )?;
        writeln!(
            f,
            "  next collection threshold: {}",
            formatted_size(self.next_collection_threshold)
        )?;
        writeln!(f, "  normal pages count: {}", self.normal_pages_count)?;
        writeln!(
            f,
            "  normal pages size: {}",
            formatted_size(self.normal_pages_size)
        )?;
        writeln!(f, "  large pages count: {}", self.large_pages_count)?;
        writeln!(
            f,
            "  large pages size: {}",
            formatted_size(self.large_pages_size)
        )?;
        writeln!(f, "  sweep pages count: {}", self.sweep_pages_count)?;
        writeln!(
            f,
            "  sweep pages size: {}",
            formatted_size(self.sweep_pages_size)
        )?;
        writeln!(f, "  gc count: {}", self.gc_count)?;
        writeln!(f, "  weak ref count: {}", self.weak_ref_count)?;
        writeln!(f, "  weak map count: {}", self.weak_map_count)?;
        writeln!(f, "  finalizable count: {}", self.finalizable_count)?;
        writeln!(f, "  destructible count: {}", self.destructible_count)?;
        writeln!(f, "  queued finalizers: {}", self.queued_finalizers)?;
        Ok(())
    }
}

#[repr(transparent)]
pub struct Managed<T: ManagedObject + ?Sized> {
    ptr: NonNull<u8>,
    marker: PhantomData<*mut T>,
}

impl<T: ManagedObject + ?Sized> Managed<T> {

    pub fn header(&self) -> &ObjectHeader {
        unsafe { &*(self.ptr.as_ptr().sub(size_of::<ObjectHeader>()) as *const ObjectHeader) }
    }

    pub unsafe fn header_mut(&mut self) -> &mut ObjectHeader {
        unsafe { &mut *(self.ptr.as_ptr().sub(size_of::<ObjectHeader>()) as *mut ObjectHeader) }
    }
    pub fn vtable(&self) -> &'static VTable {
        self.header().vtable()
    }

    pub fn ptr(self) -> *const u8 {
        self.ptr.as_ptr()
    }
    
    pub fn downcast<U: 'static + ManagedObject + ?Sized>(self) -> Option<Managed<U>> {
        if self.header().vtable().type_id == TypeId::of::<U>() {
            Some(Managed {
                ptr: self.ptr,
                marker: PhantomData,
            })
        } else {
            None
        }
    }

    pub fn is<U: 'static>(self) -> bool {
        self.header().vtable().type_id == TypeId::of::<U>()
    }
}

impl<T: ManagedObject + ?Sized> Copy for Managed<T> {}
impl<T: ManagedObject + ?Sized> Clone for Managed<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ManagedObject + PartialEq<U>, U: ManagedObject> PartialEq<Managed<U>> for Managed<T> {
    fn eq(&self, other: &Managed<U>) -> bool {
        (**self).eq(&**other)
    }
}

impl<T: ManagedObject + Eq> Eq for Managed<T> {}

impl<T: ManagedObject + PartialOrd<U>, U: ManagedObject> PartialOrd<Managed<U>> for Managed<T> {
    fn partial_cmp(&self, other: &Managed<U>) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: ManagedObject + Ord> Ord for Managed<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: ManagedObject + Hash> Hash for Managed<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

impl<T: ManagedObject> Deref for Managed<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe {
            let data = self.header().data().cast::<T>();
            &*data
        }
    }
}

impl<T: ManagedObject> DerefMut for Managed<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            let data = self.header().data().cast::<T>() as *mut T;
            &mut *data
        }
    }
}
use std::fmt;
impl<T: ManagedObject + ?Sized> fmt::Pointer for Managed<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Managed({:p})", self.ptr())
    }
}
use memoffset::offset_of;
struct WeakRefInner<T: ?Sized> {
    field: *mut u8,
    marker: PhantomData<*mut T>,
}

unsafe impl<T: ?Sized> Allocation for WeakRefInner<T> {
    const HAS_WEAKPTR: bool = true;
    const WEAKREF_OFFSETS: &'static [usize] = &[offset_of!(WeakRefInner<T>, field)];
    const FINALIZE: bool = false;
    const LIGHT_FINALIZER: bool = false;
}

unsafe impl<T: ?Sized> Trace for WeakRefInner<T> {}
unsafe impl<T: ?Sized> Finalize for WeakRefInner<T> {}
impl<T: ?Sized> ManagedObject for WeakRefInner<T> {}

#[repr(transparent)]
pub struct WeakRef<T: ?Sized> {
    pointer: Managed<WeakRefInner<T>>,
}

impl<T: ?Sized> WeakRef<T> {
    pub fn upgrade(self) -> Option<Managed<T>>
    where
        T: ManagedObject,
    {
        let ptr = self.pointer.field;
        if ptr.is_null() {
            None
        } else {
            unsafe {
                Some(Managed {
                    ptr: NonNull::new_unchecked(ptr),
                    marker: Default::default(),
                })
            }
        }
    }

    pub fn is_null(self) -> bool {
        self.pointer.field.is_null()
    }

    pub fn map<R>(self, clos: impl FnOnce(Managed<T>) -> R) -> Option<R>
    where
        T: ManagedObject,
    {
        self.upgrade().map(clos)
    }

    pub fn map_or<R>(self, default: R, clos: impl FnOnce(Managed<T>) -> R) -> R
    where
        T: ManagedObject,
    {
        self.upgrade().map_or(default, clos)
    }

    pub fn map_or_else<R, F>(self, default: F, clos: impl FnOnce(Managed<T>) -> R) -> R
    where
        T: ManagedObject,
        F: FnOnce() -> R,
    {
        self.upgrade().map_or_else(default, clos)
    }

    pub fn expect(self, msg: &str) -> Managed<T>
    where
        T: ManagedObject,
    {
        self.upgrade().expect(msg)
    }

    pub fn unwrap(self) -> Managed<T>
    where
        T: ManagedObject,
    {
        self.upgrade()
            .expect("called `WeakRef::unwrap()` but strong reference is dead")
    }

    pub fn unwrap_or_else<F>(self, f: F) -> Managed<T>
    where
        F: FnOnce() -> Managed<T>,
        T: ManagedObject,
    {
        self.upgrade().unwrap_or_else(f)
    }

    pub unsafe fn unwrap_unchecked(self) -> Managed<T>
    where
        T: ManagedObject,
    {
        self.upgrade().unwrap_unchecked()
    }

    pub fn ok_or<E>(self, err: E) -> Result<Managed<T>, E>
    where
        T: ManagedObject,
    {
        self.upgrade().ok_or(err)
    }

    pub fn ok_or_else<E, F>(self, err: F) -> Result<Managed<T>, E>
    where
        T: ManagedObject,
        F: FnOnce() -> E,
    {
        self.upgrade().ok_or_else(err)
    }

    pub fn and<U>(self, other: WeakRef<U>) -> Option<Managed<U>>
    where
        T: ManagedObject,
        U: ManagedObject,
    {
        self.upgrade().and(other.upgrade())
    }

    pub fn and_then<U, F>(self, f: F) -> Option<Managed<U>>
    where
        T: ManagedObject,
        U: ManagedObject,
        F: FnOnce(Managed<T>) -> Option<Managed<U>>,
    {
        self.upgrade().and_then(f)
    }

    pub fn filter<P>(self, predicate: P) -> Option<Managed<T>>
    where
        T: ManagedObject,
        P: FnOnce(Managed<T>) -> bool,
    {
        self.upgrade().filter(|obj| predicate(*obj))
    }

    pub fn or(self, optb: WeakRef<T>) -> Option<Managed<T>>
    where
        T: ManagedObject,
    {
        self.upgrade().or(optb.upgrade())
    }

    pub fn or_else(self, f: impl FnOnce() -> WeakRef<T>) -> Option<Managed<T>>
    where
        T: ManagedObject,
    {
        self.upgrade().or_else(|| f().upgrade())
    }

    pub fn xor(self, optb: WeakRef<T>) -> Option<Managed<T>>
    where
        T: ManagedObject,
    {
        self.upgrade().xor(optb.upgrade())
    }
}

unsafe impl<T: ?Sized> Trace for WeakRef<T> {
    fn trace(&self, visitor: &mut dyn visitor::Visitor) {
        self.pointer.trace(visitor);
    }
}

impl<T> Copy for WeakRef<T> {}
impl<T> Clone for WeakRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

/// Field of a structure that points to a weak reference. Should always be used only inside
/// structures.
pub struct WeakField<T: ?Sized> {
    pointer: *mut u8,
    marker: PhantomData<T>,
}

impl<T: ManagedObject + ?Sized> WeakField<T> {
    pub fn upgrade(&self) -> Option<Managed<T>> {
        unsafe {
            let ptr = self.pointer;
            if ptr.is_null() {
                None
            } else {
                Some(Managed {
                    ptr: NonNull::new_unchecked(ptr),
                    marker: PhantomData,
                })
            }
        }
    }

    pub fn is_null(&self) -> bool {
        self.pointer.is_null()
    }

    pub unsafe fn new(object: Managed<T>) -> Self {
        Self {
            pointer: object.ptr.as_ptr(),
            marker: PhantomData,
        }
    }
}

impl<T: ManagedObject> Managed<MaybeUninit<T>> {
    pub unsafe fn assume_init(mut self) -> Managed<T> {
        (*self.header_mut()).set_initialized();

        Managed {
            ptr: self.ptr,
            marker: PhantomData,
        }
    }

    pub fn write(&mut self, val: T) {
        (&mut **self).write(val);
    }

    pub fn as_ptr(&self) -> *const T {
        (&**self).as_ptr()
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        (&mut **self).as_mut_ptr()
    }
}

unsafe impl<T: Allocation> Allocation for MaybeUninit<T> {
    const FINALIZE: bool = false;
    const LIGHT_FINALIZER: bool = false;
}
unsafe impl<T: Trace> Finalize for MaybeUninit<T> {
    fn finalize(&mut self) {
        unreachable!()
    }
}

unsafe impl<T: Finalize> Trace for MaybeUninit<T> {
    fn trace(&self, _visitor: &mut dyn visitor::Visitor) {
        unreachable!()
    }
}

impl<T: ManagedObject> ManagedObject for MaybeUninit<T> {}
