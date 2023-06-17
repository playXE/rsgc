# Internals

RSGC is actually quite simple concurrent mark and sweep garbage collector. This file describes its internals and how it works. 

## Heap layout

Heap is one big chunk of memory divided into regions. Region size is determinated automatically on startup based on paramaters passed to the collector and amount of available RAM, although manual configuration is possible by using `GC_REGION_SIZE`, `GC_MAX_REGION_SIZE`, `GC_MIN_REGION_SIZE` and `GC_NUM_REGIONS` environment variables. By default, heap tries to use 50% of available RAM and divide it into ~2048 regions e.g on 8GB system you would get 4GB heap divided into 2048 regions of 2MB each.

Regions itself can be in four states:
- Regular: region is used for allocating from free-list inside it
- Humongous: region is used as a single humongous object. Note that if object is larger than region size, it will be allocated in multiple regions.
- Empty: region is empty and can be used for allocation, it can be uncommited and not present in RAM.
- Trash: Region that was just sweeped and is waiting to be reused.

## Allocation

RSGC has essentially 3 ways to allocate memory for objects:

### Thread Local Allocation Buffer (TLAB)

TLAB is a small buffer that is allocated for each thread and is used for allocating small objects. It is used to reduce contention on heap lock and improve allocation speed. TLAB is allocated from heap and is freed when thread dies. TLAB size is 1/8 of region size. Or manually configured by `GC_TLAB_SIZE`, `GC_MAX_TLAB_SIZE`, `GC_MIN_TLAB_SIZE` environment variables. Also `GC_ELASTIC_TLAB` can be used to enable elastic TLABs, which will use largest chunk of memory from free-list bound on `GC_MAX_TLAB_SIZE` for TLAB allocation.

### Free-list

Free List is usually used to allocate TLABs, not objects directly. It is stored per-region and divided into size-classes based on region size. When memory is requested allocation is performed by using largest fit algorithm. If there is no free memory in free-list, region is marked as full and new region is searched to fit allocation request. Objects are generally not allocated inside free-list, but in rare cases when heap is full or fragmentation is too high, resulting in no space for new TLABs, objects can be allocated directly from free-list. This has negative impact on performance, but is necessary to prevent out of memory errors.

## Bitmap allocation

Bitmap allocation is used to allocate humongous objects. They are usually determined to be 100% of region size (controlled by `GC_HUMONGOUS_THRESHOLD`, in percents). When humongous object is allocated, region is marked as humongous and is not used for regular allocation anymore. If allocation is not possible then OOM panic is triggered. 

## Metadata layout

- Mark bitmap is stored in large side-table. 
- Live bitmap for seacrhing for live objects is stored per region.
- Region metadata such as free-list, live bitmap etc is stored in side-array instead of directly in region memory mapping. 

### Object layout

Object layout is very simple. It is just a pointer to VTable containing [Object](src/system/object.rs) methods used for object scanning during GC. Also there is a separate bit for identifying that object has no references and can be skipped during marking. All objects are aligned to 16 bytes.

# Concurrent GC Cycle

GC is performed concurrently to mutator threads. It is triggered once heuristic is met. There are two heuristics available for triggering GC cycle: Static and Adaptive. First one will simply trigger GC after N bytes of memory allocated after last GC cycle. Second one has "learning" phase, where it triggers GC always on 40% full heap. Once that is done, 
it adjusts itself to met allocation rate and spikes based on each GC cycle collection time and pauses. Adapative heuristic is continously adjusted and is more suitable for long running applications.

## Concurrent Marking

Concurrent Marking has three phases to it: initial mark, concurrent mark and final mark. Initial mark is performed by stopping all mutator threads and marking all roots. Concurrent mark is performed by scanning all objects in heap and marking them. Final mark is performed by stopping all mutator threads again and processing rotos again. 

### Parallel array marking

Parallel array marking is used to speed up marking of large arrays. It is performed by splitting array into chunks and processing them in parallel. It is used for arrays larger than 2048 elements.

### Write barriers

RSGC uses Snapshot At The Beginning (SATB) write barrier. You must insert calls to `Thread::write_barrier()` before each store of heap reference to another heap reference. 

Yuasa deletion barrier algorithm is used. It is implemented as follows:
```py
def write_barrier(obj):
    if mark_in_progress: 
        if not obj.marked :
            write_barrier_queue.add(obj)
```

This barrier captures white<-black writes. `write_barrier_queue` in our case is per-thread Sequential store buffer (SSB). It is flushed once full to global write barrier queue.

## Concurrent Sweeping

Concurrent Sweeping simply walks through all regions and frees unused memory into free-list. It is performed concurrently to mutator threads. Note that regions are not available to mutator threads until sweep phase is finished.

## GC Failure

Concurrent GC can fail if it can't keep up with allocation rate. In that case, GC will try to perform full GC cycle by cancelling concurrent cycle tasks, which will stop all mutator threads and perform full GC cycle. Full GC cycle is the simplest mark&sweep cycle. 

## Rooting

Roots are automatically discovered by scanning stack and registers of each mutator thread. 

Note that if you store objects in non-heap memory you should manually register them as roots using `Heap::add_root()` method. 
Objects inside heap objects should be processed inside either `Object::trace` or `Object::trace_range` (the latter is used for parallel array processing).

