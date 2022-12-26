# RSGC

Semi-conservative Mark&Sweep Garbage Collector inspired by Shenandoah for Rust written in Rust.

## Features
- Semi-conservative: can find GC pointers on thread stack and registers
- Supports variable-sized objects (not Rust `dyn` objects!) like arrays and strings. 
- Light finalizers: cannot revive objects when an object has `DESTRUCTIBLE`` set to true.
- Finalizers: ordered finalizers that can revive objects.
- Weak references
- Weak maps
- Concurrent
- Multi-threaded

# Benchmarks

To run benchmarks you need .NET and Java installed, enter `benchmarks/binarytrees` directory and run `make`.

# Architecture

## Allocation

Allocation happens in regions, if allocation request size is larger than humongous object threshold then multiple regions are used to allocate object, otherwise object gets allocated in free-list. Fast path also includes per-thread TLAB for speeding up allocation. 

## Collection Cycle

RSGC essentially has 3 GCs: Concurrent, Degenerated and Full GC. Concurrent GC gets periodically scheduled by background controller thread. If too much allocation happens while concurrent GC runs or OOM happens we switch to degenerated GC. Degenerated GC simply finished Concurrent GC work in stop-the-world phase. If too many degenerated cycles happen we start Full GC. Full GC does GC cycle fully in stop-the-world. 

### When GC starts?

It depends on heuristics choosen. At the moment only `adaptive` and `compact` are available.

- Adaptive: these heuristics will tune GC threshold based on throughput and latency of collections. Also they account for allocation spikes that can happen at mutator time.
- Compact: simple heuristics that trigger when certain allocation or free threshold is reached. 

## Concurrency

RSGC is Concurrent Mark-And-Sweep. This means you need to ensure that objects are properly traced by collector when marking is running. For this `Thread::write_barrier` should be invoked right after write of field to an object:
```rust

fn foo(thread: &mut Thread, obj: Handle<Obj>, field: Handle<Obj2>) {
    obj.as_mut().field = field;
    thread.write_barrier(obj); // possibly push obj to mark worklist
}
```

## Safepoints

Safepoints are the only way GC can synchronize all threads at some point in time to perform some operations that can't be performed concurrently with threads running. For safepoint to happen you should invoke `Thread::safepoint`:
```rust

fn some_expensive_func(thread: &mut Thread) {
    thread.safepoint(); // safepoint at entry

    while /* some condition */ {
        // safepoint at loop header
        thread.safepoint();
        /* some expensive code */
    }
}
```


## Rooting

You do not need to somehow root your objects. GC is able to discover objects on stack automatically. You can use `Heap::add_global_root` to add walk-able root object in case not all your objects reside on stack.