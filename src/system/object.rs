use core::fmt;
use std::{
    any::{Any, TypeId},
    borrow::BorrowMut,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem::{size_of, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicPtr, AtomicU64, AtomicU32},
};

use atomic::Ordering;

use crate::{
    utils::bitfield::BitField,
    heap::{
        align_usize, atomic_cmpxchg_weak, atomic_load, atomic_store, free_list::Entry,
        thread::Thread,
    },
};

use super::traits::*;

/// Virtual GC table.
///
/// Used by GC to:
/// - Trace object properties
/// - Finalize object
/// - Process weak references inside object
/// - Obtain length of variable sized objects
#[repr(C)]
pub struct VTable {
    /// Statically known object size
    pub size: usize,
    /// Is this object variable-sized?
    pub is_varsize: bool,
    /// Tells GC how to allocate and deal with variable-sized object. Read [VarSize] documentation for more detail.
    pub varsize: VarSize,
    /// Tells GC how to trace object properties. Read [Object] documentation for more detail.
    pub trace: fn(*mut (), visitor: &mut dyn Visitor),
    /// Tells GC how to destruct object.
    pub destructor: fn(*mut ()),
    /// Tells GC how to finalize object.
    pub finalize: fn(*mut ()),

    /// Set to true when type finalizer cannot revive object i.e when finalizer is equal to `T::drop`
    pub destructible: bool,
    /// Finalizer that requires ordering of finalziation.
    pub ordered_finalizer: bool,
    /// TypeId of managed object
    pub type_id: TypeId,
    /// Type name of managed object
    pub type_name: &'static str,
    pub weak_process: fn(*mut (), &mut dyn WeakProcessor),
    /// Custom user vtable.
    pub user_vtable: *const (),
}

/// Tells GC how to allocate and deal with variable-sized object.
pub struct VarSize {
    pub is_varsize: bool,
    pub should_trace: bool,
    /// Size of item in a variable sized object
    pub itemsize: usize,
    /// Offset of length field in a variable sized object. Used by GC to trace array
    pub offset_of_length: usize,
    /// Offset of length field in a variable sized object. Used by GC to determine object size
    pub offset_of_capacity: usize,
    /// Offset of variable part of object. Used by GC to get pointer to variable part of object
    pub offset_of_variable_part: usize,

    pub iterate_range: Option<fn(*mut (), usize, usize, &mut dyn Visitor)>,
}

pub trait ConstVal<T> {
    const VAL: T;
}

pub struct VT<T> {
    marker: PhantomData<*const T>,
}


/// Tells GC how to allocate objet on the heap. In the case of non-variable sized object you can simply 
/// do `impl Allocation for MyObject {}` and it will work. For variable sized objects you need to specify
/// additional information. See constants of this trait for more detail.
pub trait Allocation: Object + Sized {
    /// Does this object need to be finalized?
    /// 
    /// NOTE: Not used right now, reserved for future use.
    const FINALIZE: bool = false;
    /// Does this object need to be destructed?
    /// 
    /// NOTE: Not used right now, reserved for future use.
    const DESTRUCTIBLE: bool = std::mem::needs_drop::<Self>();
    /// The size of this object in bytes.
    const SIZE: usize = std::mem::size_of::<Self>();
    /// Is this object variable sized? If so, GC will actually use [VARSIZE_ITEM_SIZE] and [VARSIZE_OFFSETOF_CAPACITY] and [SIZE] constants to determine object size. 
    const VARSIZE: bool = false;
    /// Size of item in a variable sized object
    const VARSIZE_ITEM_SIZE: usize = 0;
    /// Offset of length field in a variable sized object. Used by GC to trace array chunks in parallel.
    /// 
    /// Note that the field must be `u32`. GC does not support lengths larger than `u32::MAX`. Can be equal to `VARSIZE_OFFSETOF_CAPACITY` if 
    /// your object is immutable in terms of length.
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    /// Offset of capacity field in a variable sized object. Used by GC to determine object size.
    /// 
    /// Note that the field must be `u32`. GC does not support lengths larger than `u32::MAX`.
    const VARSIZE_OFFSETOF_CAPACITY: usize = 0;

    /// Offset of variable part of object. Used by GC to get pointer to variable part of object.
    /// 
    /// Must be the last field of the object like:
    /// ```
    /// #[repr(C)]
    /// struct Array {
    ///     length: u32,
    ///     _pad: [u8; 4],
    ///     data: [u8; 0],
    /// }
    /// ```
    const VARSIZE_OFFSETOF_VARPART: usize = 0;

    /// Does this object contain any heap pointers?
    const NO_HEAP_PTRS: bool = false && !Self::VARSIZE_NO_HEAP_PTRS;
    /// Does this object contain any heap pointers in a variable-sized part of it?
    const VARSIZE_NO_HEAP_PTRS: bool = false;
    /// Pointer to user vtable. 
    const USER_VTABLE: *const () = core::ptr::null();
}

impl<T: 'static + Allocation> ConstVal<&'static VTable> for VT<T> {
    const VAL: &'static VTable = &VTable {
        size: T::SIZE,
        user_vtable: T::USER_VTABLE,
        destructor: {
            fn erased<T: Object>(obj: *mut ()) {
                unsafe {
                    let obj = obj as *mut T;
                    core::ptr::drop_in_place(obj);
                }
            }
            erased::<T>
        },
        varsize: VarSize {
            is_varsize: T::VARSIZE,
            should_trace: !T::VARSIZE_NO_HEAP_PTRS,
            iterate_range: if T::VARSIZE && !T::VARSIZE_NO_HEAP_PTRS {
                Some({
                    fn erased<T: Object>(
                        obj: *mut (),
                        from: usize,
                        to: usize,
                        visitor: &mut dyn Visitor,
                    ) {
                        unsafe {
                            let obj = obj as *mut T;
                            (*obj).trace_range(from, to, visitor);
                        }
                    }
                    erased::<T>
                })
            } else {
                None
            },
            itemsize: T::VARSIZE_ITEM_SIZE,
            offset_of_length: T::VARSIZE_OFFSETOF_LENGTH,
            offset_of_capacity: T::VARSIZE_OFFSETOF_CAPACITY,
            offset_of_variable_part: T::VARSIZE_OFFSETOF_VARPART,
        },
        is_varsize: T::VARSIZE,
        trace: {
            fn erased<T: Object>(obj: *mut (), visitor: &mut dyn Visitor) {
                unsafe {
                    let obj = obj as *mut T;
                    (*obj).trace(visitor);
                }
            }

            erased::<T>
        },
        finalize: {
            fn erased<T: Object>(obj: *mut ()) {
                unsafe {
                    let obj = obj as *mut T;
                    (*obj).finalize();
                }
            }
            erased::<T>
        },
        destructible: T::DESTRUCTIBLE,
        ordered_finalizer: !T::DESTRUCTIBLE && T::FINALIZE,
        type_id: TypeId::of::<T>(),
        type_name: std::any::type_name::<T>(),
        weak_process: {
            fn erased<T: Object>(obj: *mut (), processor: &mut dyn WeakProcessor) {
                let obj = unsafe { &mut *(obj as *mut T) };
                obj.process_weak(processor);
            }

            erased::<T>
        },
    };
}

pub const NO_GC_PTRS: usize = 0;
pub const VISITED: usize = 1;
pub const FINALIZATION_ORDERING: usize = 2;

pub const SIZE_TAG_POS: usize = FINALIZATION_ORDERING + 1;
pub const SIZE_TAG_SIZE: usize = 13;
pub const VTABLE_TAG_POS: usize = SIZE_TAG_POS + SIZE_TAG_SIZE;
pub const VTABLE_TAG_SIZE: usize = 48;

pub type SizeBits = BitField<SIZE_TAG_SIZE, SIZE_TAG_POS, false>;

pub struct SizeTag;

impl SizeTag {
    pub const MAX_SIZE_TAG_IN_UNITS_OF_ALIGNMENT: u64 = ((1 << SIZE_TAG_SIZE as u64) - 1);
    pub const MAX_SIZE_TAG: u64 = Self::MAX_SIZE_TAG_IN_UNITS_OF_ALIGNMENT * 16 as u64;
    #[inline]
    pub fn decode(tag: u64) -> usize {
        Self::tag_value_to_size(SizeBits::decode(tag))
    }
    #[inline]
    pub fn encode(size: usize) -> u64 {
        SizeBits::encode(Self::size_to_tag_value(size))
    }
    #[inline]
    pub fn update(tag: u64, size: usize) -> u64 {
        SizeBits::update(Self::size_to_tag_value(size), tag)
    }
    #[inline]
    fn tag_value_to_size(tag: u64) -> usize {
        (tag as usize) << 4
    }
    #[inline]
    pub fn size_fits(size: usize) -> bool {
        size <= Self::MAX_SIZE_TAG as usize
    }
    #[inline]
    fn size_to_tag_value(size: usize) -> u64 {
        if !Self::size_fits(size) {
            0
        } else {
            (size as u64) >> 4
        }
    }
}

pub type VtableTag = BitField<VTABLE_TAG_SIZE, VTABLE_TAG_POS, false>;
pub type VisitedTag = BitField<1, VISITED, false>;
pub type NoHeapPtrsTag = BitField<1, NO_GC_PTRS, false>;
pub type FinalizationOrderingTag = BitField<1, FINALIZATION_ORDERING, false>;

/// HeapObjectHeader contains meta data per object and is prepended to each
/// object.
///
/// It stores this data:
/// - vtable: 48 bits, it is a pointer to [VTable].
/// - visited tag: 1 bit that indicates wheter object is marked or not.
/// - no heap pointers tag: 1 bit that indicates wheter object contains heap pointers or not.
/// - finalization ordering tag: 1 bit that indicates wheter object is in finalization queue or not.
pub struct HeapObjectHeader {
    pub word: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    Grey,
    White,
    Black,
}

impl HeapObjectHeader {
    pub fn should_trace(&self) -> bool {
        !self.no_heap_ptrs() || self.vtable().varsize.should_trace
    }

    #[inline]
    pub fn no_heap_ptrs(&self) -> bool {
        NoHeapPtrsTag::decode(self.word) != 0
    }
    #[inline]
    pub fn set_no_heap_ptrs(&mut self) {
        self.word = NoHeapPtrsTag::update(1, self.word);
    }
    #[inline]
    pub fn finalization_ordering(&self) -> bool {
        FinalizationOrderingTag::decode(self.word) != 0
    }
    #[inline]
    pub fn set_finalization_ordering(&mut self, value: bool) {
        self.word = FinalizationOrderingTag::update(value as u64, self.word);
    }

    #[inline]
    pub fn vtable(&self) -> &'static VTable {
        unsafe {
            let value = VtableTag::decode(self.word);
            std::mem::transmute::<usize, &'static VTable>(value as usize)
        }
    }

    #[inline]
    pub fn is_marked(&self) -> bool {
        VisitedTag::decode(self.word) != 0
    }

    #[inline]
    pub fn clear_marked(&mut self) {
        self.word = VisitedTag::update(0, self.word);
    }

    #[inline]
    pub fn set_marked(&mut self) {
        self.word = VisitedTag::update(1, self.word);
    }

    #[inline]
    pub fn is_free(&self) -> bool {
        VtableTag::decode(self.word) == 0
    }
    #[inline]
    pub fn set_heap_size(&mut self, size: usize) {
        self.word = SizeTag::update(self.word, size);
    }
    pub fn heap_size(&self) -> usize {
        let tags = self.word;

        let result = SizeTag::decode(tags);
        if result != 0 {
            return result;
        }

        unsafe { self.heap_size_from_vtable(tags) }
    }
    #[inline(always)]
    pub fn set_vtable(&mut self, vt: usize) {
        self.word = VtableTag::update(vt as _, self.word);
    }

    pub fn word_atomic(&self) -> &AtomicU64 {
        unsafe { std::mem::transmute(&self.word) }
    }

    #[inline]
    pub fn try_mark(&self) -> bool {
        let word = self
            .word_atomic()
            .load(std::sync::atomic::Ordering::Relaxed);

        if VisitedTag::decode(word) != 0 {
            return false;
        }

        let new_word = VisitedTag::update(1, word);
        self.word_atomic()
            .store(new_word, std::sync::atomic::Ordering::Relaxed);
        true
    }

    pub unsafe fn heap_size_from_vtable(&self, tags: u64) -> usize {
        let vt = VtableTag::decode(tags);
        if vt == 0 {
            let entry = self as *const Self as *const Entry;
            unsafe {
                return (*entry).heap_size();
            }
        }
        let vt = std::mem::transmute::<usize, &'static VTable>(vt as usize);

        let mut result = vt.size;
        if vt.is_varsize {
            let data = self.data();
            result += data.add(vt.varsize.offset_of_capacity).cast::<u32>().read() as usize
                * vt.varsize.itemsize;
        }
        align_usize(result + size_of::<Self>(), 16)
    }
    #[inline]
    pub fn data(&self) -> *const u8 {
        (self as *const Self as usize + size_of::<Self>()) as _
    }
    #[inline]
    pub fn data_mut(&mut self) -> *mut u8 {
        (self as *mut Self as usize + size_of::<Self>()) as _
    }

    pub fn visit(&mut self, visitor: &mut dyn Visitor) {
        if self.no_heap_ptrs() {
            return;
        }

        assert!(
            VtableTag::decode(self.word) != 0,
            "null vtable for object {:p}",
            self
        );
        let vt = self.vtable();

        (vt.trace)(self.data() as _, visitor);
    }

    pub fn visit_pointers(&mut self, visitor: &mut dyn Visitor) -> usize {
        self.visit(visitor);
        if let Some(iter) = self.vtable().varsize.iterate_range {
            let start = 0;
            let end = self.array_length();
            iter(self.data() as _, start, end, visitor);
        }
        self.heap_size()
    }
    #[inline]
    pub fn contains(&self, addr: usize) -> bool {
        let this_size = self.heap_size();
        let this_addr = self as *const Self as usize;
        (addr >= this_addr) && (addr < (this_addr + this_size))
    }

    pub fn array_length(&self) -> usize {
        let start = self.data();
        let varsize = &self.vtable().varsize;
        let length = unsafe { &*start.add(varsize.offset_of_length).cast::<AtomicU32>() };

        length.load(Ordering::Relaxed) as _
    }

    pub fn array_capacity(&self) -> usize {
        let start = self.data();
        let varsize = &self.vtable().varsize;
        let capacity = unsafe { start.add(varsize.offset_of_capacity).cast::<u32>().read() };

        capacity as _
    }
}


/// A garbage collected pointer to a value.
///
/// This is the equivalent of a garbage collected smart-pointer.
/// 
/// The smart pointer is simply a guarantee to the garbage collector
/// that this points to a garbage collected object with the correct header,
/// and not some arbitrary bits that you've decided to heap allocate.
///
/// ## Safety
/// A `Handle` can be safely transmuted back and forth from its corresponding pointer.
pub struct Handle<T: Object + ?Sized> {
    pub(crate) ptr: NonNull<u8>,
    marker: PhantomData<NonNull<T>>,
}

unsafe impl<T: Send + Object + ?Sized> Send for Handle<T> {}
unsafe impl<T: Sync + Object + ?Sized> Sync for Handle<T> {}

impl<T: Object + ?Sized> Handle<T> {
    /// Get a raw pointer to the allocation. This is the same as `as_ptr`.
    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Get immutable reference to value of type `T`. 
    #[inline]
    pub fn as_ref(&self) -> &T
    where
        T: Sized,
    {
        unsafe { self.ptr.cast::<T>().as_ref() }
    }

    /// Get mutable reference to value of type `T`.
    #[inline]
    pub fn as_mut(&mut self) -> &'_ mut T
    where
        T: Sized,
    {
        unsafe { self.ptr.cast().as_mut() }
    }

    /// Construct a `Handle` from a raw pointer.
    #[inline]
    pub unsafe fn from_raw(mem: *mut u8) -> Self {
        Self {
            ptr: NonNull::new_unchecked(mem),
            marker: PhantomData,
        }
    }

    /// Get the vtable for this object.
    #[inline]
    pub fn vtable(&self) -> &'static VTable {
        unsafe {
            let entry = self.ptr.as_ptr().cast::<HeapObjectHeader>().sub(1);
            (*entry).vtable()
        }
    }

    /// Cast this handle to a dyn handle. Note that it is not a fat-pointer. RTTI required
    /// to downcast back to a concrete type is stored in GC header *behind* the object. 
    #[inline]
    pub fn as_dyn(this: &Self) -> Handle<dyn Object> {
        Handle {
            ptr: this.ptr,
            marker: Default::default(),
        }
    }


    /// Check if this handle is a handle to an object of type `U`.
    #[inline]
    pub fn is<U: Object + ?Sized + 'static>(&self) -> bool {
        self.vtable().type_id == TypeId::of::<U>()
    }

    /// Compares equality of two handles. This is a pointer equality check.
    #[inline]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.ptr == other.ptr
    }

    /// Replace the value of this handle with another value.
    #[inline]
    pub fn replace(&mut self, other: T) -> T
    where T: Sized
    {
        std::mem::replace(&mut**self, other)
    }
}

impl Handle<dyn Object> {

    /// Downcast this handle to a handle of type `U`.
    pub fn downcast<U: Object + Sized + 'static>(self) -> Option<Handle<U>> {
        if self.vtable().type_id == TypeId::of::<U>() {
            Some(Handle {
                ptr: self.ptr,
                marker: Default::default(),
            })
        } else {
            None
        }
    }

    /// Downcast this handle to a reference of type `U`.
    pub fn downcast_ref<U: Object + Sized + 'static>(&self) -> Option<&U> {
        if self.vtable().type_id == TypeId::of::<U>() {
            Some(unsafe { self.ptr.cast::<U>().as_ref() })
        } else {
            None
        }
    }

    /// Downcast this handle to a mutable reference of type `U`.
    pub fn downcast_mut<U: Object + Sized + 'static>(&mut self) -> Option<&mut U> {
        if self.vtable().type_id == TypeId::of::<U>() {
            Some(unsafe { self.ptr.cast::<U>().as_mut() })
        } else {
            None
        }
    }
}

impl<T: Object + Sized> Handle<MaybeUninit<T>> {
    /// Assume that the value is initialized and returns a handle to it.
    pub unsafe fn assume_init(self) -> Handle<T> {
        Handle {
            ptr: self.ptr,
            marker: PhantomData,
        }
    }
}

impl<T: Object + ?Sized> Copy for Handle<T> {}
impl<T: Object + ?Sized> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Object + ?Sized> fmt::Pointer for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle({:p})", self.ptr)
    }
}

impl<T: Object> Object for MaybeUninit<T> {}
impl<T: Allocation + Object> Allocation for MaybeUninit<T> {
    const FINALIZE: bool = T::FINALIZE;
    const DESTRUCTIBLE: bool = T::DESTRUCTIBLE;
    const SIZE: usize = T::SIZE;
    const USER_VTABLE: *const () = T::USER_VTABLE;
    const VARSIZE: bool = T::VARSIZE;
    const VARSIZE_ITEM_SIZE: usize = T::VARSIZE_ITEM_SIZE;
    const VARSIZE_OFFSETOF_LENGTH: usize = T::VARSIZE_OFFSETOF_LENGTH;
    const VARSIZE_OFFSETOF_VARPART: usize = T::VARSIZE_OFFSETOF_VARPART;
}

impl<T: Object + ?Sized> Allocation for Handle<T> {
    const NO_HEAP_PTRS: bool = false;
}

impl<T: Object + Sized> Deref for Handle<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T: Object + Sized> DerefMut for Handle<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
    }
}

impl<T: Object + fmt::Debug> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl<T: Object + fmt::Display> fmt::Display for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl<T: Object + Hash> Hash for Handle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_ref().hash(state)
    }
}
use std::borrow::Borrow;
impl<T: Object + Borrow<U>, U> AsRef<U> for Handle<T> {
    fn as_ref(&self) -> &U {
        self.as_ref().borrow()
    }
}

impl<T: Object + BorrowMut<U>, U> AsMut<U> for Handle<T> {
    fn as_mut(&mut self) -> &mut U {
        self.as_mut().borrow_mut()
    }
}

impl<T: Object + PartialEq<U>, U: Object + PartialEq<T>> PartialEq<Handle<U>> for Handle<T> {
    fn eq(&self, other: &Handle<U>) -> bool {
        self.as_ref().eq(other.as_ref())
    }
}

impl<T: Object + Eq> Eq for Handle<T> {}

impl<T: Object + PartialOrd<U>, U: Object + PartialOrd<T>> PartialOrd<Handle<U>> for Handle<T> {
    fn partial_cmp(&self, other: &Handle<U>) -> Option<std::cmp::Ordering> {
        self.as_ref().partial_cmp(other.as_ref())
    }
}

impl<T: Object + Ord> Ord for Handle<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_ref().cmp(other.as_ref())
    }
}

impl Object for () {}
impl Allocation for () {}

impl<A: Object, B: Object> Object for (A, B) {
    fn trace(&self, visitor: &mut dyn Visitor) {
        self.0.trace(visitor);
        self.1.trace(visitor);
    }
}
impl<A: Object + Allocation, B: Object + Allocation> Allocation for (A, B) {
    const SIZE: usize = A::SIZE + B::SIZE;
    const NO_HEAP_PTRS: bool = A::NO_HEAP_PTRS && B::NO_HEAP_PTRS;
}

impl<A: Object, B: Object, C: Object> Object for (A, B, C) {
    fn trace(&self, visitor: &mut dyn Visitor) {
        self.0.trace(visitor);
        self.1.trace(visitor);
        self.2.trace(visitor);
    }
}
impl<A: Object + Allocation, B: Object + Allocation, C: Object + Allocation> Allocation for (A, B, C) {
    const SIZE: usize = A::SIZE + B::SIZE + C::SIZE;
    const NO_HEAP_PTRS: bool = A::NO_HEAP_PTRS && B::NO_HEAP_PTRS && C::NO_HEAP_PTRS;
}

