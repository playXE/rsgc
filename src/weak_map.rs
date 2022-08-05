use std::{collections::{HashMap, hash_map::RandomState}, hash::BuildHasher};

use crate::{memory::{object_header::ObjectHeader, traits::{Trace, ManagedObject, Finalize, Allocation}, Heap}, Managed};

pub struct WeakMap<V: Trace,S: Trace = RandomState > {
    map: HashMap<*mut ObjectHeader, V,S >
}
impl<V: Trace, S: Trace> WeakMap<V, S> {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn capacity(&self) -> usize {
        self.map.capacity()
    }

    pub fn set<K: ManagedObject + ?Sized>(&mut self, object: Managed<K>, value: V) -> Option<V>
    where S: BuildHasher
    {
        self.map.insert(object.ptr() as _, value)
    }

    pub fn get<K: ManagedObject + ?Sized>(&self, object: Managed<K>) -> Option<&V>
    where S: BuildHasher
    {
        self.map.get(&(object.ptr() as *mut ObjectHeader))
    }

    pub fn get_mut<K: ManagedObject + ?Sized>(&mut self, object: Managed<K>) -> Option<&mut V>
    where S: BuildHasher
    {
        self.map.get_mut(&(object.ptr() as *mut ObjectHeader))
    }

    pub fn with_hasher(heap: &mut Heap, hash_builder: S) -> Managed<Self> 
    where 
        V: 'static,
        S: 'static
    {
        let map = HashMap::<*mut ObjectHeader, V, S>::with_hasher(hash_builder);
        allocate(heap, map)
    }

    pub fn with_capacity_and_hasher(heap: &mut Heap, capacity: usize, hash_builder: S) -> Managed<Self> 
    where 
        V: 'static,
        S: 'static
    {
        let map = HashMap::<*mut ObjectHeader, V, S>::with_capacity_and_hasher(capacity, hash_builder);
        allocate(heap, map)
    }
}

impl<V: Trace> WeakMap<V, RandomState> {
    pub fn new(heap: &mut Heap) -> Managed<Self>  where V: 'static{
        allocate::<V,RandomState>(heap, HashMap::<*mut ObjectHeader, V>::new())
    }

    pub fn with_capacity(heap: &mut Heap, capacity: usize) -> Managed<Self>  where V: 'static{
        allocate::<V,RandomState>(heap, HashMap::with_capacity(capacity))
    }
}

fn allocate<K: 'static +Trace, S: 'static + Trace>(heap: &mut Heap, map: HashMap<*mut ObjectHeader, K,S>) -> Managed<WeakMap<K,S>> {
    let map = heap.manage(WeakMap { map });
    unsafe { heap.add_weak_map(map); }
    map
}

impl<K: Trace, S: Trace> ManagedObject for WeakMap<K,S> {}

unsafe impl<V: Trace, S: Trace> Allocation for WeakMap<V,S> {
    const FINALIZE: bool = true;
    const LIGHT_FINALIZER: bool = true;
    const __IS_WEAKMAP: bool = true;
}

unsafe impl<V: Trace, S: Trace> Finalize for WeakMap<V, S> {
    fn __weak_map_process(&mut self, callback: &mut dyn FnMut(*mut ObjectHeader) -> *mut ObjectHeader) {
        self.map.retain(|key, _| {
            let object = *key;
            let new_object = callback(object);
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