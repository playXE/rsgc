# rsgc

Semi-conservative Mark&Sweep Garbage Collector for Rust written in Rust.

## Features
- Semi-conservative: can find GC pointers on thread stack and registers
- Supports variable-sized objects (not Rust `dyn` objects!) like arrays and strings. 
- Light finalizers: cannot revive objects when an object has `LIGHT_FINALIZER`` set to true.
- Finalizers: ordered finalizers that can revive objects.
- Weak references
- Weak maps

## Allocator

rsgc uses page based allocation scheme. Small enough objects are allocated from the segregated free list in 128KB pages. 
Large objects get an individual page for allocation that is best sized for it. Allocation is also sped up by using an inline allocation buffer that simply does bump-pointer allocation. 