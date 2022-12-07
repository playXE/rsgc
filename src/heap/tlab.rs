use std::ptr::null_mut;

use crate::heap::bitmap::HeapBitmap;
use super::heap::heap;


// ThreadLocalAllocBuffer: a descriptor for thread-local storage used by
// the threads for allocation.
//            It is thread-private at any time, but maybe multiplexed over
//            time across multiple threads. The park()/unpark() pair is
//            used to make it available for such multiplexing.
pub struct ThreadLocalAllocBuffer {
    start: usize,
    top: usize,
    end: usize,

    pub(crate) bitmap: *const HeapBitmap<16>,

    desired_size: usize,
}

impl ThreadLocalAllocBuffer {
    pub const fn new() -> Self {
        Self {
            start: 0,
            top: 0,
            end: 0,
            desired_size: 0,
            bitmap: null_mut(),
        }
    }

    #[inline(always)]
    pub fn allocate(&mut self, size: usize) -> *mut u8 {
        let obj = self.top;

        if self.end - obj >= size {
            // succesfull thread-local allocation

            self.top = obj + size;
            unsafe { (*self.bitmap).set_atomic(obj); }
            unsafe {
                debug_assert_eq!((*self.bitmap).find_object_start(obj) as usize, obj)
            }
            return obj as _;
        }

        null_mut()
    }

    pub fn initialize_(&mut self, start: usize, top: usize, end: usize) {
        self.start = start;
        self.end = end;
        self.top = top;
        self.bitmap = unsafe {
            let heap = heap();
            let ix = heap.region_index(start as _);
            let region = heap.get_region(ix);
            &(*region).object_start_bitmap
        };
    }


    pub fn initialize(&mut self) {
        self.initialize_(0, 0, 0);

        self.desired_size = heap().options().max_tlab_size;
    }

    pub fn end(&self) -> usize {
        self.end
    }

    /// Invoked before GC cycle to mark free memory as actually free.
    pub fn retire(&mut self) {
        if self.end() != 0 {
            unsafe {
                self.free_remaining();
            }
            self.top = 0;
            self.end = 0;
            self.start = 0;
            self.bitmap = null_mut();
        }
    }

    unsafe fn free_remaining(&mut self) {
        let heap = heap();

        let region_ix = heap.region_index(self.start as *mut u8);
        let region = heap.get_region(region_ix);
        if self.end - self.top != 0 {
            (*region).free_list.add(self.top as *mut u8, self.end - self.top);
        }
    }
}

