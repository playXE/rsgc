use crate::{
    block_allocator::BlockAllocator,
    constants::{LINE_COUNT, LINE_SIZE, WORDS_IN_LINE},
    meta::{
        block::{get_block_end, get_block_start, get_line_address, BlockMeta},
        line::FreeLineMeta,
    },
    utils::bytemap::Bytemap, heap::Heap,
};

#[repr(C)]
pub struct Allocator {
    pub block_meta_start: *mut usize,
    pub bytemap: *mut Bytemap,
    pub block_allocator: *mut BlockAllocator,
    pub heap_start: *mut usize,
    pub block: *mut BlockMeta,
    pub block_start: *mut usize,
    pub cursor: *mut usize,
    pub limit: *mut usize,
    pub large_block: *mut BlockMeta,
    pub large_block_start: *mut usize,
    pub large_cursor: *mut usize,
    pub large_limit: *mut usize,
}

impl Allocator {
    pub fn new(
        block_meta_start: *mut usize,
        bytemap: *mut Bytemap,
        block_allocator: *mut BlockAllocator,
        heap_start: *mut usize,
    ) -> Self {
        Self {
            block_meta_start,
            bytemap,
            block_allocator,
            heap_start,
            block: core::ptr::null_mut(),
            block_start: core::ptr::null_mut(),
            cursor: core::ptr::null_mut(),
            limit: core::ptr::null_mut(),
            large_block: core::ptr::null_mut(),
            large_block_start: core::ptr::null_mut(),
            large_cursor: core::ptr::null_mut(),
            large_limit: core::ptr::null_mut(),
        }
    }

    pub fn can_init_cursors(&self) -> bool {
        unsafe {
            let free_block_count = (*self.block_allocator).free_block_count;
            free_block_count >= 2
                || (free_block_count == 1 && (*self.block_allocator).recycled_blocks_count > 0)
        }
    }

    pub unsafe fn init_cursors(&mut self) {
        let mut large_block;
        let mut retries = 0;
        loop {
            let did_init = self.new_block();
            large_block = (*self.block_allocator).get_free_block();
           
            if did_init && !large_block.is_null() {
                break;
            }
            if retries < 5 {
                Heap::get().collect();
                retries += 1;
                continue;
            }
            panic!("Out of memory");
        }
        
        self.large_block = large_block;
        self.large_block_start =
            get_block_start(self.block_meta_start, self.heap_start, large_block);
        self.large_cursor = self.large_block_start;
        self.large_limit = get_block_end(self.large_block_start);
       
    }

    /// Overflow allocation uses only free blocks, it is used when the bump limit of
    /// the fast allocator is too small to fit
    /// the block to alloc.
    #[inline(never)]
    pub unsafe fn overflow_allocation(&mut self, size: usize) -> *mut usize {
        let start = self.large_cursor;
        let end = start.cast::<u8>().add(size).cast::<usize>();

        if end > self.large_limit {
            let block = (*self.block_allocator).get_free_block();

            if block.is_null() {
                return core::ptr::null_mut();
            }

            let block_start = get_block_start(self.block_meta_start, self.heap_start, block);
            self.large_block = block;
            self.large_block_start = block_start;
            self.large_cursor = block_start;
            self.large_limit = get_block_end(block_start);

            return self.overflow_allocation(size);
        }

        core::ptr::write_bytes(start.cast::<u8>(), 0, size);

        self.large_cursor = end;

        start
    }

    pub unsafe fn alloc(&mut self, size: usize) -> *mut usize {
        let start = self.cursor;
        let end = start.cast::<u8>().add(size).cast::<usize>();

        if end > self.limit {
            if size > LINE_SIZE {
                return self.overflow_allocation(size);
            } else {
                if self.get_next_line() {
                    return self.alloc(size);
                }

                return core::ptr::null_mut();
            }
        }

        core::ptr::write_bytes(start.cast::<u8>(), 0, size);
        self.cursor = end;
        start
    }
    #[inline(never)]
    pub unsafe fn get_next_line(&mut self) -> bool {
        let block = self.block;
        let block_start = self.block_start;

        let line_index = (*block).first_free_line();

        if line_index == -1 {
            
            return self.new_block();
        }

        let line = get_line_address(block_start, line_index as _);

        self.cursor = line;
        let line_meta = line.cast::<FreeLineMeta>();

        (*block).set_first_free_line((*line_meta).next);

        let size = (*line_meta).size;
        self.limit = self.cursor.add(size as usize * WORDS_IN_LINE);

        assert!(self.limit <= get_block_end(block_start));

        true
    }

    pub unsafe fn new_block(&mut self) -> bool {
        let mut block = (*self.block_allocator).try_recycled();
        let block_start;
        if !block.is_null() {
            block_start = get_block_start(self.block_meta_start, self.heap_start, block);

            let line_index = (*block).first_free_line() as usize;
          
            assert!(line_index < LINE_COUNT, "{} {}", line_index, LINE_COUNT as i8);
            let line = get_line_address(block_start, line_index as _);

            self.cursor = line;
            let line_meta = line.cast::<FreeLineMeta>();

            (*block).set_first_free_line((*line_meta).next);
            let size = (*line_meta).size;
            assert!(size > 0);
            self.limit = self.cursor.add(size as usize * WORDS_IN_LINE);
        } else {
            block = (*self.block_allocator).get_free_block();

            if block.is_null() {
                
                return false;
            }

            block_start = get_block_start(self.block_meta_start, self.heap_start, block);

            self.cursor = block_start;
            self.limit = get_block_end(block_start);

            (*block).set_first_free_line(-1);
        }

        self.block = block;
        self.block_start = block_start;

        true
    }
}
