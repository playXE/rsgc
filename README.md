# rsgc

Semi-conservative Mark&Sweep Garbage Collector for Rust written in Rust.

## Features
- Semi-conservative: can find GC pointers on thread stack and registers
- Supports variable-sized objects (not Rust `dyn` objects!) like arrays and strings. 
- Light finalizers: cannot revive objects when an object has `LIGHT_FINALIZER`` set to true.
- Finalizers: ordered finalizers that can revive objects.
- Weak references
- Weak maps
- Moving


## Multi-threading

RSGC allows to have multiple threads accessing objectS in the heap but only one mutator can allocate objects. This limitation comes from the allocator that is tuned for single-threaded allocation performance and specific use case: JS engine where only one mutator always exists although helper threads that might need to access objects could be spawned.