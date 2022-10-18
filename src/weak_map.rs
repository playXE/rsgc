use std::{
    collections::{hash_map::RandomState, HashMap},
    hash::BuildHasher,
};

use crate::{
    memory::{
        object_header::ObjectHeader,
        traits::{Allocation, Finalize, ManagedObject, Trace, WeakMapProcessor},
        Heap,
    },
    Managed,
};

/// Collection of key/value pairs whose keys must be objects, with values of type `V` and 
/// which does not create strong references to the keys. That is, an object's presence as a 
/// key in a WeakMap does not prevent the object from being garbage collected.
/// Once an object used as a key has been collected, its corresponding values in 
/// any WeakMap become candidates for garbage collection as well — as long as they aren't strongly referred to elsewhere.
/// 
/// WeakMap allows associating data to objects in a way that doesn't prevent the key objects from being collected,
/// even if the values reference the keys. However, a WeakMap doesn't allow observing the liveness of its keys, 
/// which is why it doesn't allow enumeration; if a WeakMap exposed any method to obtain a list of its keys, 
/// the list would depend on the state of garbage collection, introducing non-determinism.
pub struct WeakMap<V: Trace, S: Trace = RandomState> {
    map: HashMap<*mut ObjectHeader, V, S>,
}
impl<V: Trace, S: Trace> WeakMap<V, S> {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn capacity(&self) -> usize {
        self.map.capacity()
    }

    pub fn has<K: ManagedObject + ?Sized>(&self, object: Managed<K>) -> bool 
    where S: BuildHasher
    {
        self.get(object).is_some()
    }

    pub fn set<K: ManagedObject + ?Sized>(&mut self, object: Managed<K>, value: V) -> Option<V>
    where
        S: BuildHasher,
    {
        self.map.insert(object.ptr() as _, value)
    }

    pub fn get<K: ManagedObject + ?Sized>(&self, object: Managed<K>) -> Option<&V>
    where
        S: BuildHasher,
    {
        self.map.get(&(object.ptr() as *mut ObjectHeader))
    }

    pub fn get_mut<K: ManagedObject + ?Sized>(&mut self, object: Managed<K>) -> Option<&mut V>
    where
        S: BuildHasher,
    {
        self.map.get_mut(&(object.ptr() as *mut ObjectHeader))
    }

    pub fn with_hasher(heap: &mut Heap, hash_builder: S) -> Managed<Self>
    where
        V: 'static,
        S: 'static,
    {
        let map = HashMap::<*mut ObjectHeader, V, S>::with_hasher(hash_builder);
        allocate(heap, map)
    }

    pub fn with_capacity_and_hasher(
        heap: &mut Heap,
        capacity: usize,
        hash_builder: S,
    ) -> Managed<Self>
    where
        V: 'static,
        S: 'static,
    {
        let map =
            HashMap::<*mut ObjectHeader, V, S>::with_capacity_and_hasher(capacity, hash_builder);
        allocate(heap, map)
    }
}

impl<V: Trace> WeakMap<V, RandomState> {
    pub fn new(heap: &mut Heap) -> Managed<Self>
    where
        V: 'static,
    {
        allocate::<V, RandomState>(heap, HashMap::<*mut ObjectHeader, V>::new())
    }

    pub fn with_capacity(heap: &mut Heap, capacity: usize) -> Managed<Self>
    where
        V: 'static,
    {
        allocate::<V, RandomState>(heap, HashMap::with_capacity(capacity))
    }
}

fn allocate<K: 'static + Trace, S: 'static + Trace>(
    heap: &mut Heap,
    map: HashMap<*mut ObjectHeader, K, S>,
) -> Managed<WeakMap<K, S>> {
    let map = heap.manage(WeakMap { map });
    unsafe {
        heap.add_weak_map(map);
    }
    map
}

impl<K: Trace, S: Trace> ManagedObject for WeakMap<K, S> {}

unsafe impl<V: Trace, S: Trace> Allocation for WeakMap<V, S> {
    const FINALIZE: bool = true;
    const LIGHT_FINALIZER: bool = true;
    const __IS_WEAKMAP: bool = true;
}

unsafe impl<V: Trace, S: Trace> Finalize for WeakMap<V, S> {
    fn __weak_map_process(&mut self, callback: &mut WeakMapProcessor<'_>) {
        self.map.retain(|key, _| {
            let object = *key;
            let new_object = callback.process(object);
            if new_object.is_null() {
                false
            } else {
                true
            }
        });
    }
}

unsafe impl<V: Trace, S: Trace> Trace for WeakMap<V, S> {
    fn trace(&self, visitor: &mut dyn crate::memory::visitor::Visitor) {
        for (_, value) in self.map.iter() {
            value.trace(visitor);
        }

        self.map.hasher().trace(visitor);
    }
}

unsafe impl Trace for RandomState {}
