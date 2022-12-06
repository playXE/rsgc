#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemoryRegion {
    pointer: *mut u8,
    size: usize,
}

impl MemoryRegion {
    pub fn start(&self) -> usize {
        self.pointer as _
    }

    pub const fn new(pointer: *mut u8, size: usize) -> Self {
        Self { pointer, size }
    }

    pub fn pointer(&self) -> *mut u8 {
        self.pointer
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn set_size(&mut self, size: usize) {
        self.size = size;
    }

    pub fn load<T>(&self, offset: usize) -> T {
        unsafe { self.compute_internal_pointer::<T>(offset).read() }
    }

    pub fn store<T>(&self, offset: usize, value: T) {
        unsafe { self.compute_internal_pointer::<T>(offset).write(value) }
    }

    pub fn store_unaligned<T>(&self, offset: usize, value: T) {
        unsafe {
            self.compute_internal_pointer::<T>(offset)
                .write_unaligned(value)
        }
    }

    pub fn pointer_to<T>(&self, offset: usize) -> *mut T {
        self.compute_internal_pointer::<T>(offset)
    }

    pub fn contains(&self, address: usize) -> bool {
        (address >= self.start()) && (address < self.end())
    }

    pub fn end(&self) -> usize {
        self.start() + self.size
    }

    pub fn subregion(&mut self, from: &MemoryRegion, offset: usize, size: usize) {
        self.pointer = (from.start() + offset) as _;
        self.size = size;
    }

    pub fn extend(&mut self, region: &MemoryRegion, extra: usize) {
        self.pointer = region.pointer();
        self.size = region.size() + extra;
    }

    pub fn copy_from(&mut self, offset: usize, from: &MemoryRegion) {
        unsafe {
            core::ptr::copy(
                from.pointer(),
                (self.start() + offset) as *mut u8,
                from.size(),
            );
        }
    }

    fn compute_internal_pointer<T>(&self, offset: usize) -> *mut T {
        (self.start() + offset) as _
    }
}
