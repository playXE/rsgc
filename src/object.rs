use core::fmt;
use std::{
    any::{Any, TypeId},
    marker::PhantomData,
    mem::{size_of, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::{null_mut, NonNull},
    sync::atomic::{AtomicPtr, AtomicU64},
};

use crate::{
    bitfield::BitField,
    heap::{align_usize, atomic_load, atomic_store, free_list::Entry, thread::Thread},
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
    /// Tells GC how to trace object properties. Read [Trace] documentation for more detail.
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
    /// Offset of length field in a variable sized object. Used by GC to determine object size
    pub offset_of_length: usize,
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

pub trait Allocation: Object + Sized {
    const FINALIZE: bool = false;
    const DESTRUCTIBLE: bool = std::mem::needs_drop::<Self>();
    const SIZE: usize = std::mem::size_of::<Self>();
    const VARSIZE: bool = false;
    const VARSIZE_ITEM_SIZE: usize = 0;
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    const VARSIZE_OFFSETOF_VARPART: usize = 0;
    const NO_HEAP_PTRS: bool = false;
    const VARSIZE_NO_HEAP_PTRS: bool = false;
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
            should_trace: T::VARSIZE_NO_HEAP_PTRS,
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

/// ObjectHeader contains meta data per object and is prepended to each
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
    #[inline]
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
            result += data.add(vt.varsize.offset_of_length).cast::<u32>().read() as usize
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
        let length = unsafe { start.add(varsize.offset_of_length).cast::<u32>().read() };

        length as _
    }
}

pub struct Handle<T: Object + ?Sized> {
    ptr: NonNull<u8>,
    marker: PhantomData<NonNull<T>>,
}

impl<T: Object + ?Sized> Handle<T> {
    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    #[inline]
    pub fn as_ref(&self) -> &T
    where
        T: Sized,
    {
        unsafe { self.ptr.cast::<T>().as_ref() }
    }

    #[inline]
    pub fn as_mut(&mut self) -> &'_ mut T
    where
        T: Sized,
    {
        unsafe { self.ptr.cast().as_mut() }
    }

    #[inline]
    pub unsafe fn from_raw(mem: *mut u8) -> Self {
        Self {
            ptr: NonNull::new_unchecked(mem),
            marker: PhantomData,
        }
    }

    #[inline]
    pub fn vtable(&self) -> &'static VTable {
        unsafe {
            let entry = self.ptr.as_ptr().cast::<HeapObjectHeader>().sub(1);
            (*entry).vtable()
        }
    }

    #[inline]
    pub fn as_dyn(this: &Self) -> Handle<dyn Object> {
        Handle {
            ptr: this.ptr,
            marker: Default::default(),
        }
    }
    #[inline]
    pub fn make_atomic(this: &Self) -> AtomicHandle<T> {
        AtomicHandle {
            ptr: this.ptr.as_ptr(),
            marker: Default::default(),
        }
    }
}

impl Handle<dyn Object> {
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
}

impl<T: Object + Sized> Handle<MaybeUninit<T>> {
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

#[repr(C)]
pub struct Array<T: Object + Sized> {
    length: u32,
    init_length: u32,
    data: [T; 0],
}

impl<T: 'static + Object + Sized> Array<T> {
    pub fn new(th: &mut Thread, len: usize, mut init: impl FnMut(usize) -> T) -> Handle<Self> where T: Allocation {
        let mut result = th.allocate_varsize::<Self>(len);
        let arr = result.as_mut().as_mut_ptr();
        for i in 0..len {
            unsafe {
                let data = (*arr).data.as_mut_ptr().add(i);
                data.write(init(i));
                (*arr).init_length += 1;
            }
        }
        unsafe { result.assume_init() }
    }
}

impl<T: Object + ?Sized> Object for Handle<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.ptr.as_ptr());
    }
}

impl<T: Object> Object for Array<T> {
    fn finalize(&mut self) {
        unsafe {
            core::ptr::drop_in_place(std::slice::from_raw_parts_mut(
                self.data.as_mut_ptr(),
                self.init_length as _,
            ));
        }
    }

    fn trace_range(&self, from: usize, to: usize, visitor: &mut dyn Visitor) {
        unsafe {
            let slice = std::slice::from_raw_parts(self.data.as_ptr().add(from), to - from);
            for val in slice.iter() {
                val.trace(visitor);
            }
        }
    }

    fn trace(&self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }

    fn process_weak(&mut self, processor: &mut dyn WeakProcessor) {
        unsafe {
            let slice =
                std::slice::from_raw_parts_mut(self.data.as_mut_ptr(), self.init_length as _);

            for val in slice {
                val.process_weak(processor);
            }
        }
    }
}

impl<T: Object + Sized + Allocation> Allocation for Array<T> {
    const FINALIZE: bool = std::mem::needs_drop::<T>();
    const DESTRUCTIBLE: bool = true;
    const SIZE: usize = size_of::<Self>();
    const NO_HEAP_PTRS: bool = true;
    const VARSIZE_NO_HEAP_PTRS: bool = T::NO_HEAP_PTRS;
    const VARSIZE: bool = true;
    const VARSIZE_ITEM_SIZE: usize = size_of::<T>();
    const VARSIZE_OFFSETOF_LENGTH: usize = 0;
    const VARSIZE_OFFSETOF_VARPART: usize = size_of::<usize>();
}

pub struct AtomicHandle<T: Object + ?Sized> {
    ptr: *mut u8,
    marker: PhantomData<*const T>,
}

impl<T: Object + ?Sized> AtomicHandle<T> {
    pub fn null() -> Self {
        Self {
            ptr: null_mut(),
            marker: PhantomData,
        }
    }

    pub fn load(&self, order: atomic::Ordering) -> Option<Handle<T>> {
        Some(Handle {
            ptr: NonNull::new(atomic_load(&self.ptr, order))?,
            marker: PhantomData,
        })
    }

    pub fn store(&self, val: Option<Handle<T>>, order: atomic::Ordering) {
        atomic_store(
            &self.ptr,
            val.map(|o| o.ptr.as_ptr()).unwrap_or(null_mut()),
            order,
        )
    }

    pub fn is_null(&self) -> bool {
        self.load_relaxed().is_none()
    }

    pub fn load_relaxed(&self) -> Option<Handle<T>> {
        self.load(atomic::Ordering::Relaxed)
    }

    pub fn store_relaxed(&self, val: Option<Handle<T>>) {
        self.store(val, atomic::Ordering::Relaxed);
    }

    pub fn load_acquire(&self) -> Option<Handle<T>> {
        self.load(atomic::Ordering::Acquire)
    }

    pub fn store_release(&self, val: Option<Handle<T>>) {
        self.store(val, atomic::Ordering::Release);
    }

}

impl<T: Object + ?Sized> Clone for AtomicHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Object + ?Sized> Copy for AtomicHandle<T> {}
unsafe impl<T: Object + ?Sized> Send for AtomicHandle<T> {}
unsafe impl<T: Object + ?Sized> Sync for AtomicHandle<T> {}

impl<T: Object + ?Sized> Object for AtomicHandle<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        if let Some(handle) = self.load_acquire() {
            visitor.visit(handle.ptr.as_ptr());
        }
    }
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
