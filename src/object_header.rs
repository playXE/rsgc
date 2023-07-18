use std::ptr::NonNull;

use crate::{
    constants::{
        ALLOCATION_ALIGNMENT, ALLOCATION_ALIGNMENT_INVERSE_MASK, ALLOCATION_ALIGNMENT_WORDS,
        LARGE_BLOCK_MASK, MIN_BLOCK_SIZE, WORD_SIZE,
    },
    heap::Heap,
    meta::{
        block::{
            get_block_meta, get_block_start, get_block_start_for_word, get_humongous_start,
            BlockMeta,
        },
        line::LineFlag,
        object::ObjectMeta,
    },
    traits::{Allocation, Object, Visitor},
    utils::math::round_to_next_multiple,
};

pub struct RTTI {
    pub size: usize,
    pub varsize: Option<VarSize>,
    pub weak_offsets: Option<&'static [usize]>,
    pub trace: fn(*mut (), &mut dyn Visitor),
    pub finalize: fn(*mut ()),
}

#[derive(Copy, Clone, PartialEq, PartialOrd)]
pub struct VarSize {
    pub length_offset: usize,
    pub capacity_offset: usize,
    pub itemsize: usize,
}

#[repr(C)]
pub struct ObjectHeader {
    pub rtti: &'static RTTI,
}

impl ObjectHeader {
    #[inline]
    pub const fn is_array(&self) -> bool {
        self.rtti.varsize.is_some()
    }

    pub fn object_start(&self) -> *const u8 {
        unsafe { (self as *const ObjectHeader).add(1).cast() }
    }

    #[inline]
    pub unsafe fn size(&self) -> usize {
        let mut sz = self.rtti.size;
        
        if let Some(varsize) = self.rtti.varsize {
            let length = self
                .object_start()
                .add(varsize.capacity_offset)
                .cast::<usize>()
                .read();
            sz += varsize.itemsize * length;
        }

        round_to_next_multiple(sz, ALLOCATION_ALIGNMENT)
    }

    #[inline]
    pub const fn is_weak_reference(&self) -> bool {
        self.rtti.weak_offsets.is_some()
    }

    pub unsafe fn get_last_word(this: *mut Self) -> *mut usize {
        let size = (*this).size();
     
        let last = (this.cast::<u8>().add(size))
            .cast::<usize>()
            .sub(ALLOCATION_ALIGNMENT_WORDS);
        last
    }

    pub unsafe fn get_inner_pointer(
        heap: &mut Heap,
        block_meta: *mut BlockMeta,
        word: *mut usize,
        word_meta: *mut ObjectMeta,
    ) -> *mut Self {
        let stride;
        let block_start;

        if (*block_meta).contains_large_objects() {
            stride = MIN_BLOCK_SIZE / ALLOCATION_ALIGNMENT;
            let super_block_start = get_humongous_start(heap.block_meta_start, block_meta);
            block_start =
                get_block_start(heap.block_meta_start, heap.heap_start, super_block_start);
        } else {
            stride = 1;
            block_start = get_block_start_for_word(word);
        }

        let mut current = word;

        let mut current_meta = word_meta;

        while current >= block_start && current_meta.read() == ObjectMeta::Free {
            current = current.sub(ALLOCATION_ALIGNMENT_WORDS * stride);
            current_meta = current_meta.sub(stride);
        }

        let object = current.cast::<Self>();

        if current_meta.read() == ObjectMeta::Allocated
            && word < current.add((*object).size() / WORD_SIZE)
        {
            return object;
        } else {
            return core::ptr::null_mut();
        }
    }

    pub unsafe fn get_unmarked_object(heap: &mut Heap, mut word: *mut usize) -> *mut Self {
        let block_meta = get_block_meta(heap.block_meta_start, heap.heap_start, word);
        
        if (*block_meta).contains_large_objects() {
            word = (word as usize & LARGE_BLOCK_MASK) as _;
        } else {
            word = (word as usize & ALLOCATION_ALIGNMENT_INVERSE_MASK) as _;
        }
       
        let word_meta = &mut *(*heap.bytemap).get(word);

        if matches!(word_meta, ObjectMeta::Placeholder | ObjectMeta::Marked) {
            return core::ptr::null_mut();
        } else if matches!(word_meta, ObjectMeta::Allocated) {
            
            return word.cast();
        } else {
            
            return Self::get_inner_pointer(heap, block_meta, word, word_meta);
        }
    }

    pub unsafe fn mark(heap: &mut Heap, object: *mut Self, object_meta: *mut ObjectMeta) {
        object_meta.write(ObjectMeta::Marked);

        let block_meta = get_block_meta(heap.block_meta_start, heap.heap_start, object.cast());
       
        if !(*block_meta).contains_large_objects() {
            (*block_meta).mark();
            
            let last_word = Self::get_last_word(object);

            let first_line_meta = heap.get_line_meta_for_word(object.cast());
            let last_line_meta = heap.get_line_meta_for_word(last_word);
            
            let mut line_meta = first_line_meta;

            while line_meta <= last_line_meta {
                
                line_meta.cast::<LineFlag>().write(LineFlag::Marked);
                line_meta = line_meta.add(1);
            }
        }
    }
}

pub trait ConstVal<T> {
    const VAL: T;
}

pub struct RTTIOf<T: Allocation + Object>(core::marker::PhantomData<T>);

impl<T: Allocation + Object> ConstVal<&'static RTTI> for RTTIOf<T> {
    const VAL: &'static RTTI = &RTTI {
        size: T::SIZE,
        varsize: if T::VARSIZE {
            Some(VarSize {
                length_offset: T::VARSIZE_OFFSETOF_LENGTH,
                capacity_offset: T::VARSIZE_OFFSETOF_CAPACITY,
                itemsize: T::VARSIZE_ITEMSIZE,
            })
        } else {
            None
        },
        trace: {
            fn trace<T: Allocation + Object>(this: *mut (), visitor: &mut dyn Visitor) {
                T::trace(unsafe { this.cast::<T>().as_ref().unwrap() }, visitor)
            }

            trace::<T>
        },
        finalize: {
            fn finalize<T: Allocation + Object>(this: *mut ()) {
                unsafe { core::ptr::drop_in_place(this.cast::<T>()) }
            }

            finalize::<T>
        },
        weak_offsets: T::WEAK_OFFSETS,
    };
}



#[repr(transparent)]
pub struct Handle<T: Object> {
    pub(crate) ptr: NonNull<T>,
    pub(crate) marker: core::marker::PhantomData<T>,
}

impl<T: Object> std::ops::Deref for Handle<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: Object> std::ops::DerefMut for Handle<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: Object> AsRef<T> for Handle<T> {
    fn as_ref(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: Object> AsMut<T> for Handle<T> {
    fn as_mut(&mut self) -> &mut T {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: Object> Copy for Handle<T> {}
impl<T: Object> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Object> std::fmt::Pointer for Handle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.ptr.fmt(f)
    }
}

unsafe impl<T: Object> Object for Handle<T> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        unsafe {
            visitor.visit(self.ptr.as_ptr().cast::<ObjectHeader>().sub(1).cast());
        }
    }
}