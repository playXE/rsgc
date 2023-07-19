use std::{
    borrow::Borrow,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use crate::{
    prelude::{Allocation, Handle, Object, Thread},
    system::array::Array,
};

pub struct HashNode<K: Object, V: Object> {
    pub hash: u64,
    pub key: K,
    pub value: Option<V>,
    pub next: Option<Handle<Self>>,
}

unsafe impl<K: Object, V: Object> Object for HashNode<K, V> {
    fn trace(&self, visitor: &mut dyn crate::prelude::Visitor) {
        self.key.trace(visitor);
        self.value.trace(visitor);
        if let Some(next) = self.next {
            next.trace(visitor);
        }
    }
}

unsafe impl<K: Object, V: Object> Allocation for HashNode<K, V> {}

pub struct HashMap<K: Object, V: Object> {
    count: u32,
    nodes: Handle<Array<Option<Handle<HashNode<K, V>>>>>,
}

unsafe impl<K: Object, V: Object> Object for HashMap<K, V> {
    fn trace(&self, visitor: &mut dyn crate::prelude::Visitor) {
        self.nodes.trace(visitor);
    }
}

unsafe impl<K: Object, V: Object> Allocation for HashMap<K, V> {}

impl<K: Object, V: Object> HashMap<K, V> {
    pub fn with_capacity(thread: &mut Thread, mut cap: usize) -> Handle<Self> {
        if cap < 16 {
            cap = 16;
        }

        if cap >= u32::MAX as usize {
            panic!("Capacity overflow");
        }
        let arr = Array::<Option<Handle<HashNode<K, V>>>>::new(thread, cap, |_, _| None);

        thread.allocate(Self {
            count: 0,
            nodes: arr,
        })
    }

    pub fn new(thread: &mut Thread) -> Handle<Self> {
        Self::with_capacity(thread, 16)
    }
}

impl<K: Hash + PartialEq + Object + std::fmt::Debug, V: Object> HashMap<K, V> {
    fn rehash_buckets(
        t: &mut Thread,
        src: Handle<Array<Option<Handle<HashNode<K, V>>>>>,
        mut dst: Handle<Array<Option<Handle<HashNode<K, V>>>>>,
    ) {
        for i in 0..src.len() {
            let mut bucket = src[i];

            while let Some(mut entry) = bucket {
                let next = entry.next;
                
                let mut hasher = DefaultHasher::new();
                entry.key.hash(&mut hasher);
                let hash = hasher.finish();
                let j = hash as usize % dst.len();
                if let Some(e) = dst[j] {
                    t.write_barrier(e);
                }
                entry.next = dst[j];
                t.write_barrier(entry);
                dst[j] = Some(entry);

                bucket = next;
            }
        }
    }

    fn resize(mut self: Handle<Self>) {
        let size = self.nodes.len() * 2;
        let t = Thread::current();
        let new_nodes = Array::<Option<Handle<HashNode<K, V>>>>::new(t, size, |_, _| None);

        Self::rehash_buckets(t, self.nodes, new_nodes);
        t.write_barrier(self);
        self.nodes = new_nodes;
    }

    pub fn put(mut self: Handle<Self>, key: K, value: V) -> Option<V> {
        let t = Thread::current();
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash = hasher.finish();

        let mut position = hash as usize % self.nodes.len();

        let mut bucket = self.nodes[position];

        while let Some(mut entry) = bucket {
            if entry.hash == hash && entry.key == key {
                t.write_barrier(entry);
                return entry.value.replace(value);
            }

            bucket = entry.next;
        }

        if self.count >= (self.nodes.len() as f64 * 0.75) as u32 {
            self.resize();
            position = hash as usize % self.nodes.len();
        }

        let node = t.allocate(HashNode {
            hash,
            key,
            value: Some(value),
            next: self.nodes[position],
        });

        self.count += 1;

        t.write_barrier(self);
        self.nodes[position] = Some(node);

        None
    }

    pub fn remove<Q>(mut self: Handle<Self>, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let t = Thread::current();

        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash = hasher.finish();

        let position = hash as usize % self.nodes.len();

        let mut bucket = self.nodes[position];
        let mut prev = None::<Handle<HashNode<K, V>>>;

        while let Some(mut entry) = bucket {
            if entry.hash == hash && entry.key.borrow() == key {
                if let Some(mut prev) = prev {
                    t.write_barrier(prev);
                    prev.next = entry.next;
                } else {
                    t.write_barrier(self);
                    self.nodes[position] = entry.next;
                }

                t.write_barrier(entry);
                return entry.value.take();
            }

            prev = Some(entry);
            bucket = entry.next;
        }

        None
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash = hasher.finish();

        let position = hash as usize % self.nodes.len();

        let mut bucket = &self.nodes[position];

        while let Some(entry) = bucket {
            if entry.hash == hash && entry.key.borrow() == key {
                return entry.value.as_ref();
            }

            bucket = &entry.next;
        }

        None
    }

    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash = hasher.finish();

        let position = hash as usize % self.nodes.len();

        let mut bucket = &mut self.nodes[position];

        while let Some(entry) = bucket {
            if entry.hash == hash && entry.key.borrow() == key {
                return entry.value.as_mut();
            }

            bucket = &mut entry.next;
        }

        None
    }

    pub fn entry<'a>(self: &'a mut Handle<Self>, key: K) -> Entry<'a, K, V> {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash = hasher.finish();

        let position = hash as usize % self.nodes.len();

        let mut bucket = self.nodes[position];

        while let Some(entry) = bucket {
            if entry.hash == hash && entry.key == key {
                return Entry::Occupied(OccupiedEntry {
                    map: self,
                    node: entry,
                });
            }

            bucket = entry.next;
        }

        Entry::Vacant(VacantEntry {
            map: self,
            key,
            hash,
        })
    }

    pub fn iter<'a>(self: &'a Handle<Self>) -> Iter<'a, K, V> {
        Iter {
            map: self,
            index: 0,
            entry: None,
        }
    }

    pub fn keys<'a>(self: &'a Handle<Self>) -> impl Iterator<Item = &'a K> {
        self.iter().map(|(key, _)| key)
    }

    pub fn values<'a>(self: &'a Handle<Self>) -> impl Iterator<Item = &'a V> {
        self.iter().map(|(_, value)| value)
    }

    pub fn len(&self) -> usize {
        self.count as usize
    }

    pub fn capacity(&self) -> usize {
        self.nodes.len()
    }
}

pub enum Entry<'a, K: Object + Hash, V: Object> {
    Occupied(OccupiedEntry<'a, K, V>),
    Vacant(VacantEntry<'a, K, V>),
}

pub struct VacantEntry<'a, K: Object + Hash, V: Object> {
    map: &'a mut Handle<HashMap<K, V>>,
    key: K,
    hash: u64,
}

impl<'a, K: 'static + Object + Hash + std::fmt::Debug, V: 'static + Object> VacantEntry<'a, K, V> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn insert(self, value: V)
    where
        K: PartialEq + Eq,
    {
        let thread = Thread::current();
        let mut tab = self.map.nodes;
        let n = tab.len();
        let i = ((n as u64 - 1) & self.hash) as usize;

        let p = tab[i as usize];

        let node = thread.allocate(HashNode {
            hash: self.hash,
            key: self.key,
            value: Some(value),
            next: p,
        });

        thread.write_barrier(tab);
        tab[i] = Some(node);

        self.map.count += 1;
        if self.map.count > (self.map.nodes.len() as f64 * 0.75) as u32 {
            self.map.resize();
        }

        /*self.map.table.as_mut().unwrap()[i as usize]
        .as_mut()
        .unwrap()
        .value
        .as_mut()
        .unwrap()*/
    }
}

pub struct OccupiedEntry<'a, K: Object, V: Object> {
    map: &'a mut Handle<HashMap<K, V>>,
    node: Handle<HashNode<K, V>>,
}

impl<'a, K: 'static + Object + std::fmt::Debug, V: 'static + Object> OccupiedEntry<'a, K, V> {
    pub fn key(&self) -> &K {
        &self.node.key
    }

    pub fn get(&self) -> &V {
        unsafe { self.node.value.as_ref().unwrap_unchecked() }
    }

    pub fn get_mut(&mut self) -> &mut V {
        unsafe { self.node.value.as_mut().unwrap_unchecked() }
    }

    pub fn insert(&mut self, value: V) -> V {
        let old = self.node.value.replace(value);
        old.unwrap()
    }

    pub fn remove(&mut self) -> V
    where
        K: Hash + Eq,
    {
        let old = self.node.value.take();
        self.map.remove(&self.node.key);
        old.unwrap()
    }
}

pub struct Iter<'a, K: Object, V: Object> {
    map: &'a HashMap<K, V>,
    index: usize,
    entry: Option<&'a Handle<HashNode<K, V>>>,
}

impl<'a, K: Object, V: Object> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(entry) = self.entry {
            let next = &entry.next;
            self.entry = next.as_ref();
            return Some((&entry.key, entry.value.as_ref().unwrap()));
        }

        while self.index < self.map.nodes.len() {
            let entry = &self.map.nodes[self.index];
            self.index += 1;

            if let Some(entry) = entry {
                self.entry = entry.next.as_ref();
                return Some((&entry.key, entry.value.as_ref().unwrap()));
            }
        }

        None
    }
}
