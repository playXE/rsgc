use crate::base::bitfield::BitField;
use crate::base::constants::*;
use crate::memory::free_list::FreeListElement;
use crate::memory::traits::*;
use crate::memory::visitor::*;
use std::marker::PhantomData;
use std::{any::TypeId, mem::size_of};

#[derive(Clone, Copy)]
pub union Tid {
    pub as_word: Word,
    pub as_vtable: &'static VTable,
}

#[cfg(target_pointer_width = "32")]
pub type Word = u32;
#[cfg(target_pointer_width = "64")]
pub type Word = u64;

/*
bitfield! {
    #[derive(Copy, Clone)]
    pub struct HDR(u64);
    #[doc = "Pointer to VTable or forwarding pointer if object is in nursery and `finalization_ordering` is set to true"]
    Word, tid, set_tid: 63,5;
    bool, track_young_ptrs,set_track_young_ptrs: 0;
    bool, no_heap_ptrs,set_no_heap_ptrs: 1;
    bool, visited,set_visited: 2;
    bool, has_shadow,set_has_shadow: 3;
    bool, finalization_ordering,set_finalization_ordering: 4;
}*/

pub struct VTable {
    pub size: usize,
    pub is_varsize: bool,
    pub varsize: VarSize,
    pub trace: fn(*mut (), visitor: &mut dyn Visitor),
    pub finalize: Option<fn(*mut ())>,
    /// Set to true when type finalizer cannot revive object i.e when finalizer is equal to `T::drop`
    pub light_finalizer: bool,
    pub type_id: TypeId,
    pub type_name: &'static str,
    pub weak_refs: &'static [usize],

    pub(crate) weak_map_process: Option<fn(*mut (), &mut dyn FnMut(*mut ObjectHeader) -> *mut ObjectHeader)>,
}

/*
quasiconst! {
    pub const VTABLE<T: 'static + Allocation>: &'static VTable = &VTable {
        size: T::SIZE,
        varsize:
            VarSize {
                itemsize:T::VARSIZE_ITEM_SIZE,
                offset_of_length: T::VARSIZE_OFFSETOF_LENGTH,
                offset_of_variable_part:T::VARSIZE_OFFSETOF_VARPART,
                field_might_be_object: T::field_might_be_object,
            },
        is_varsize: T::VARSIZE,
        trace: trace_erased::<T>,
        finalize:if T::FINALIZE { Some(finalize_erased::<T>) } else { None },
        light_finalizer: T::LIGHT_FINALIZER,
        type_id: TypeId::of::<T>(),

    };
}
*/

pub trait ConstVal<T> {
    const VAL: T;
}

pub struct VT<T> {
    marker: PhantomData<*const T>,
}

impl<T: 'static + Allocation> ConstVal<&'static VTable> for VT<T> {
    const VAL: &'static VTable = &VTable {
        size: T::SIZE,
        varsize: VarSize {
            itemsize: T::VARSIZE_ITEM_SIZE,
            offset_of_length: T::VARSIZE_OFFSETOF_LENGTH,
            offset_of_variable_part: T::VARSIZE_OFFSETOF_VARPART,
            field_might_be_object: T::field_might_be_object,
        },
        is_varsize: T::VARSIZE,
        trace: trace_erased::<T>,
        finalize: if T::FINALIZE {
            Some(finalize_erased::<T>)
        } else {
            None
        },
        light_finalizer: T::LIGHT_FINALIZER,
        type_id: TypeId::of::<T>(),
        type_name: std::any::type_name::<T>(),
        weak_refs: &T::WEAKREF_OFFSETS,
        weak_map_process: if T::__IS_WEAKMAP {
            Some(weak_map_process_erased::<T>)
        } else {
            None
        },
    };
}

pub struct VarSize {
    pub itemsize: usize,
    pub offset_of_length: usize,
    pub offset_of_variable_part: usize,
    pub field_might_be_object: fn(at: *mut u8) -> *mut ObjectHeader,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccessMode {
    Atomic,
    NonAtomic,
}

pub const NO_GC_PTRS: usize = 0;
pub const PINNED: usize = 1;
pub const VISITED: usize = 2;
pub const HAS_SHADOW: usize = 3;
pub const FINALIZATION_ORDERING: usize = 4;
pub const INITIALIZED_TAG: usize = 5;
pub const SHADOW_INITIALIZED: usize = 6;
pub const SHADOW_INITIALIZED_SIZE: usize = 1;

pub const SIZE_TAG_POS: usize = SHADOW_INITIALIZED + SHADOW_INITIALIZED_SIZE;
pub const SIZE_TAG_SIZE: usize = 8;
pub const VTABLE_TAG_POS: usize = SIZE_TAG_POS + SIZE_TAG_SIZE;
pub const VTABLE_TAG_SIZE: usize = 48;

pub type SizeBits = BitField<SIZE_TAG_SIZE, SIZE_TAG_POS, false>;

pub struct SizeTag;

impl SizeTag {
    pub const MAX_SIZE_TAG_IN_UNITS_OF_ALIGNMENT: u64 = ((1 << SIZE_TAG_SIZE as u64) - 1);
    pub const MAX_SIZE_TAG: u64 =
        Self::MAX_SIZE_TAG_IN_UNITS_OF_ALIGNMENT * OBJECT_ALIGNMENT as u64;

    pub fn decode(tag: u64) -> usize {
        Self::tag_value_to_size(SizeBits::decode(tag))
    }

    pub fn encode(size: usize) -> u64 {
        SizeBits::encode(Self::size_to_tag_value(size))
    }
    pub fn update(tag: u64, size: usize) -> u64 {
        SizeBits::update(Self::size_to_tag_value(size), tag)
    }
    fn tag_value_to_size(tag: u64) -> usize {
        (tag as usize) << OBJECT_ALIGNMENT_LOG2
    }

    pub fn size_fits(size: usize) -> bool {
        size <= Self::MAX_SIZE_TAG as usize
    }

    fn size_to_tag_value(size: usize) -> u64 {
        if !Self::size_fits(size) {
            0
        } else {
            (size as u64) >> OBJECT_ALIGNMENT_LOG2
        }
    }
}

pub type VtableTag = BitField<VTABLE_TAG_SIZE, VTABLE_TAG_POS, false>;
pub type VisitedTag = BitField<1, VISITED, false>;
pub type NoHeapPtrsTag = BitField<1, NO_GC_PTRS, false>;
pub type FinalizationOrderingTag = BitField<1, FINALIZATION_ORDERING, false>;
pub type InitializedTag = BitField<1, INITIALIZED_TAG, false>;
use crate::base::bitfield::ToAtomicBitField;

pub struct ObjectHeader {
    pub bits: u64,
}

impl ObjectHeader {
    pub fn is_initialized(&self) -> bool {
        InitializedTag::decode(self.bits) != 0
    }

    pub fn set_initialized(&mut self) {
        self.bits = InitializedTag::update(1, self.bits);
    }

    pub fn no_heap_ptrs(&self) -> bool {
        NoHeapPtrsTag::decode(self.bits) != 0
    }

    pub fn set_no_heap_ptrs(&mut self) {
        self.bits = NoHeapPtrsTag::update(1, self.bits);
    }

    pub fn finalization_ordering(&self) -> bool {
        FinalizationOrderingTag::decode(self.bits) != 0
    }

    pub fn set_finalization_ordering(&mut self, value: bool) {
        self.bits = FinalizationOrderingTag::update(value as u64, self.bits);
    }

    pub fn is_visited(&self) -> bool {
        VisitedTag::make_atomic(&self.bits).read(std::sync::atomic::Ordering::Relaxed) != 0
    }

    pub fn is_visited_unsynchronized(&self) -> bool {
        VisitedTag::decode(self.bits) != 0
    }

    pub fn set_visited(&self) {
        VisitedTag::make_atomic(&self.bits).update(1);
    }

    pub fn clear_visited(&self) -> bool {
        VisitedTag::make_atomic(&self.bits).try_clear()
    }

    pub fn clear_visited_unsync(&mut self) {
        self.bits = VisitedTag::update(0, self.bits);
    }

    pub fn try_mark(&self) -> bool {
        VisitedTag::make_atomic(&self.bits).try_acquire()
    }

    pub fn forward_word(&self) -> *mut Self {
        VtableTag::decode(self.bits) as _
    }

    pub unsafe fn array_from(&self) -> *mut u8 {
        let vt = self.vtable();
        assert!(vt.is_varsize);

        let data = self.data().add(vt.varsize.offset_of_variable_part);
        data as _
    }

    pub unsafe fn array_length(&self) -> usize {
        let vt = self.vtable();
        assert!(vt.is_varsize);

        let data = self.data().add(vt.varsize.offset_of_length);
        let length = *(data as *const usize);
        length
    }

    pub unsafe fn array_itemsize(&self) -> usize {
        let vt = self.vtable();
        assert!(vt.is_varsize);

        vt.varsize.itemsize
    }

    pub unsafe fn find_object(&mut self, visitor: &dyn FindObjectVisitor) -> bool {
        visitor.find_object(self)
    }

    pub fn is_new_object(&self) -> bool {
        let addr = self as *const Self as usize;
        (addr & OBJECT_ALIGNMENT_MASK) == NEW_OBJECT_ALIGNMENT_OFFSET
    }

    pub fn is_old_object(&self) -> bool {
        let addr = self as *const Self as usize;
        (addr & OBJECT_ALIGNMENT_MASK) == OLD_OBJECT_ALIGNMENT_OFFSET
    }

    pub fn vtable(&self) -> &'static VTable {
        unsafe {
            let value = VtableTag::decode(self.bits);
            std::mem::transmute::<usize, &'static VTable>(value as usize)
        }
    }
    pub fn set_heap_size(&mut self, size: usize) {
        self.bits = SizeTag::update(self.bits, size);
    }
    pub fn heap_size(&self) -> usize {
        let tags = self.bits;

        let result = SizeTag::decode(tags);
        if result != 0 {
            return result;
        }

        unsafe { self.heap_size_from_vtable(tags) }
    }

    pub fn set_vtable(&mut self, vt: usize) {
        self.bits = VtableTag::update(vt as _, self.bits);
    }

    pub unsafe fn heap_size_from_vtable(&self, tags: u64) -> usize {
        let vt = VtableTag::decode(tags);
        if vt == 0 {
            let element = self as *const Self as *mut FreeListElement;
            return (*element).heap_size();
        }
        let vt = std::mem::transmute::<usize, &'static VTable>(vt as usize);

        let mut result = vt.size;
        if vt.is_varsize {
            let data = self.data();
            result +=
                data.add(vt.varsize.offset_of_length).cast::<usize>().read() * vt.varsize.itemsize;
        }
        result + size_of::<Self>()
    }

    pub fn data(&self) -> *const u8 {
        (self as *const Self as usize + size_of::<Self>()) as _
    }

    pub fn data_mut(&mut self) -> *mut u8 {
        (self as *mut Self as usize + size_of::<Self>()) as _
    }

    pub fn visit(&mut self, visitor: &mut dyn Visitor) {
        if self.no_heap_ptrs() || !self.is_initialized() {
            return;
        }
        let vt = self.vtable();
        (vt.trace)(self.data() as _, visitor);
    }

    pub fn visit_pointers(&mut self, visitor: &mut dyn Visitor) -> usize {
        self.visit(visitor);

        self.heap_size()
    }

    pub fn contains(&self, addr: usize) -> bool {
        let this_size = self.heap_size();
        let this_addr = self as *const Self as usize;
        (addr >= this_addr) && (addr < (this_addr + this_size))
    }
}
