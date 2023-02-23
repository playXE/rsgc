//! HashMap implementation based on OpenJDK one
//!

use std::{
    borrow::Borrow,
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hash},
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use crate::{
    system::{
        array::Array,
        object::{Allocation, Handle},
        traits::Object,
        traits::Visitor,
    },
    thread::Thread,
};

pub struct Node<K: Object, V: Object> {
    pub(super) hash: u64,
    pub(super) key: K,
    pub(super) value: Option<V>,
    pub(super) next: Option<Handle<Node<K, V>>>,
}

impl<K: Object + Hash, V: Object + Hash> Hash for Node<K, V> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key.hash(state);
        self.value.hash(state);
    }
}

impl<K: Object, V: Object> Node<K, V> {
    pub fn new(hash: u64, key: K, value: V, next: Option<Handle<Node<K, V>>>) -> Self {
        Self {
            hash,
            key,
            value: Some(value),
            next,
        }
    }

    pub fn set_value(&mut self, value: V) {
        self.value = Some(value);
    }

    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn next(&self) -> Option<&Handle<Node<K, V>>> {
        self.next.as_ref()
    }

    pub unsafe fn set_next(&mut self, next: Option<Handle<Node<K, V>>>) {
        self.next = next;
    }
}

impl<K: Object + PartialEq, V: Object + PartialEq> PartialEq for Node<K, V> {
    fn eq(&self, other: &Self) -> bool {
        if self as *const Self == other as *const Self {
            return true;
        }

        self.key == other.key && self.value == other.value
    }
}

impl<K: Object, V: Object> Object for Node<K, V> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        self.key.trace(visitor);
        self.value.trace(visitor);
        self.next.trace(visitor);
    }
}

impl<K: Object, V: Object> Allocation for Node<K, V> {}

pub enum DefaultHashBuilder {}

pub struct HashMap<K: Object, V: Object, S = RandomState> {
    table: Option<Handle<Array<Option<Handle<Node<K, V>>>>>>,
    size: u32,
    mod_count: u32,
    threshold: u32,
    load_factor: f32,
    marker: PhantomData<*const Node<K, V>>,
    hash_builder: S,
}

impl<K: 'static + Object, V: 'static + Object, S: 'static + BuildHasher> HashMap<K, V, S>
where
    K: PartialEq + Eq,
{
    pub const DEFAULT_INITIAL_CAPACITY: u32 = 1 << 4;
    pub const MAXIMUM_CAPACITY: u32 = 1 << 30;
    pub const DEFAULT_LOAD_FACTOR: f32 = 0.75;

    pub const fn table_size_for(cap: usize) -> usize {
        let n = (-1isize) as usize >> (cap - 1).leading_zeros();

        if n >= Self::MAXIMUM_CAPACITY as usize {
            Self::MAXIMUM_CAPACITY as _
        } else {
            n as usize + 1
        }
    }

    pub fn with_hasher(hash_builder: S) -> Handle<Self> {
        Self::with_hasher_and_capacity(hash_builder, Self::DEFAULT_INITIAL_CAPACITY)
    }

    pub fn with_hasher_and_capacity(hash_builder: S, initial_capacity: u32) -> Handle<Self> {
        Self::with_hasher_and_capacity_and_load_factor(
            hash_builder,
            initial_capacity,
            Self::DEFAULT_LOAD_FACTOR,
        )
    }

    pub fn with_hasher_and_capacity_and_load_factor(
        hash_builder: S,
        mut initial_capacity: u32,
        load_factor: f32,
    ) -> Handle<Self> {
        if initial_capacity > Self::MAXIMUM_CAPACITY {
            initial_capacity = Self::MAXIMUM_CAPACITY;
        }
        if load_factor <= 0.0 || load_factor.is_nan() {
            panic!("Illegal load factor");
        }

        Thread::current().allocate(Self {
            table: None,
            size: 0,
            mod_count: 0,
            threshold: Self::table_size_for(initial_capacity as _) as _,
            load_factor,
            marker: PhantomData,
            hash_builder,
        })
    }

    pub fn capacity_from_threshold(&self) -> u32 {
        if self.threshold == 0 {
            0
        } else {
            (self.threshold as usize * 2).next_power_of_two() as u32
        }
    }

    fn get_node<Q>(&self, key: &Q) -> Option<&Node<K, V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = make_hash(&self.hash_builder, key);

        if let Some(tab) = self.table.as_ref() {
            let n = tab.len();
            if n == 0 {
                return None;
            }
            let first = &tab[(n - 1) & hash as usize];
            if let Some(first) = first {
                if first.key.borrow() == key && first.hash == hash {
                    return Some(first);
                }

                let mut e = first.next.as_ref();
                while let Some(node) = e {
                    if node.key.borrow() == key && node.hash == hash {
                        return Some(node);
                    }
                    e = node.next.as_ref();
                }
            } else {
                return None;
            }
        }

        None
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_node(key)
            .map(|node| unsafe { node.value.as_ref().unwrap_unchecked() })
    }

    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_node(key).is_some()
    }

    pub fn put<'a>(self: &'a mut Handle<Self>, thread: &mut Thread, key: K, value: V) -> Option<V>
    where
        K: Hash,
    {
        let hash = make_hash(&self.hash_builder, &key);
        self.put_val(thread, hash, key, value)
    }

    fn put_val<'a>(
        self: &'a mut Handle<Self>,
        thread: &mut Thread,
        hash: u64,
        key: K,
        value: V,
    ) -> Option<V> {
        let mut tab = match self.table {
            Some(tab) => tab,
            None => self.resize().unwrap(),
        };

        let n = tab.len();

        let i = ((n as u64 - 1) & hash) as usize;

        let mut node = tab[i];
        while let Some(mut n) = node {
            if n.hash == hash && n.key == key {
                thread.write_barrier(n);
                return n.value.replace(value);
            }
            node = n.next
        }

        let node = thread.allocate(Node {
            hash,
            key,
            value: Some(value),
            next: tab[i],
        });

        tab[i] = Some(node);

        self.mod_count += 1;
        self.size += 1;
        if self.size > self.threshold {
            println!("{} > {}", self.size, self.threshold);
            self.resize();
        }

        None
    }

    fn resize(self: &mut Handle<Self>) -> Option<Handle<Array<Option<Handle<Node<K, V>>>>>> {
        let thread = Thread::current();

        let old_tab = self.table;
        let old_cap = old_tab.as_ref().map(|tab| tab.len()).unwrap_or(0) as u32;
        let mut new_cap;
        let mut new_thr = 0;
        let old_thr = self.threshold;
        if old_cap > 0 {
            if old_cap >= Self::MAXIMUM_CAPACITY {
                self.threshold = std::u32::MAX;
                return old_tab;
            } else if (({ new_cap = old_cap << 1; new_cap }) as u32) < Self::MAXIMUM_CAPACITY
                && old_cap >= Self::DEFAULT_INITIAL_CAPACITY
            {
                new_thr = old_thr << 1;
            }
        } else if old_thr > 0 {
            new_cap = old_thr;
        } else {
            new_cap = Self::DEFAULT_INITIAL_CAPACITY;
            new_thr =
                (Self::DEFAULT_LOAD_FACTOR as f32 * Self::DEFAULT_INITIAL_CAPACITY as f32) as u32;
        }

        if new_thr == 0 {
            let ft = new_cap as f32 * self.load_factor;
            new_thr = if ft < Self::MAXIMUM_CAPACITY as f32 && new_cap < Self::MAXIMUM_CAPACITY {
                ft.round() as u32
            } else {
                std::u32::MAX
            };
        }
        self.threshold = new_thr;
        let mut newtab = Array::new(thread, new_cap as _, |_, _| None);
        thread.write_barrier(*self);
        self.table = Some(newtab);
        println!("resize: {} -> {}", old_cap, new_cap);
        if let Some(mut old_tab) = old_tab {
            for j in 0..old_cap {
                let mut e = old_tab[j as usize];
                old_tab[j as usize] = None;
                while let Some(mut n) = e {
                    e = n.next;
                    let i = (n.hash % (new_cap as u64)) as usize;
                    thread.write_barrier(newtab);
                    n.next = newtab[i];
                    newtab[i] = Some(n);
                }
            }
        }

        Some(newtab)
    }

    pub fn remove<Q>(self: &mut Handle<Self>, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = make_hash(&self.hash_builder, key);
        self.remove_node(key, hash)
            .map(|mut n| unsafe { n.value.take().unwrap_unchecked() })
    }

    fn remove_node<Q>(self: &mut Handle<Self>, key: &Q, hash: u64) -> Option<Handle<Node<K, V>>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let thread = Thread::current();
        if let Some(mut tab) = self.table {
            let n = tab.len();
            let i = ((n as u64 - 1) & hash) as usize;

            let p = tab[i as usize];

            if let Some(mut p) = p {
                let mut node = None;
                if p.hash == hash && p.key.borrow() == key {
                    node = Some(p);
                } else if p.next.is_some() {
                    let mut e = p.next;
                    while let Some(n) = e {
                        if n.hash == hash && n.key.borrow() == key {
                            node = Some(n);
                            break;
                        }
                        e = n.next;
                    }
                }

                if let Some(node) = node {
                    if Handle::ptr_eq(&node, &p) {
                        thread.write_barrier(tab);
                        tab[i] = node.next;
                    } else {
                        thread.write_barrier(p);
                        p.next = node.next;
                    }
                    self.mod_count += 1;
                    self.size -= 1;
                    return Some(node);
                }
            }
        }
        None
    }

    pub fn clear(self: &mut Handle<Self>) {
        let thread = Thread::current();

        if let Some(mut tab) = self.table {
            thread.write_barrier(tab);
            for i in 0..tab.len() {
                tab[i] = None;
            }
            self.size = 0;
            self.mod_count += 1;
        }
    }

    pub fn entry<'a>(self: &'a mut Handle<Self>, key: K) -> Entry<'a, K, V, S>
    where K: Hash + Eq + PartialEq
    {
        let hash = make_hash(&self.hash_builder, &key);
        let tab = self.table;
        let mut e = None;
        if let Some(tab) = tab {
            let i = ((tab.len() as u64 - 1) & hash) as usize;
            let mut p = tab[i];
            while let Some(n) = p {
                if n.hash == hash && n.key.borrow() == &key {
                    e = Some(n);
                    break;
                }
                p = n.next;
            }
        } else {
            self.resize().unwrap();
        }
        if let Some(e) = e {
            Entry::Occupied(OccupiedEntry { map: self, node: e })
        } else {
            Entry::Vacant(VacantEntry {
                key,
                hash,
                map: self,
            })
        }
    }
}

impl<K: Object, V: Object, S: 'static> HashMap<K, V, S> {
    pub fn len(self: &Handle<Self>) -> usize {
        self.size as _
    }

    pub fn capacity(self: &Handle<Self>) -> usize {
        if let Some(tab) = self.table {
            tab.len()
        } else {
            0
        }
    }

    pub fn iter<'a>(self: &'a Handle<Self>) -> Iter<'a, K, V, S> {
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
}

pub(crate) fn make_hash<Q, S>(hash_builder: &S, val: &Q) -> u64
where
    Q: Hash + ?Sized,
    S: BuildHasher,
{
    use core::hash::Hasher;
    let mut state = hash_builder.build_hasher();
    val.hash(&mut state);
    state.finish()
}

impl<K: Object, V: Object, S: 'static> Object for HashMap<K, V, S> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        self.table.trace(visitor);
    }
}

impl<K: Object, V: Object, S: 'static> Allocation for HashMap<K, V, S> {}

pub struct Iter<'a, K: Object, V: Object, S> {
    map: &'a HashMap<K, V, S>,
    index: usize,
    entry: Option<&'a Handle<Node<K, V>>>,
}

impl<'a, K: Object, V: Object, S> Iterator for Iter<'a, K, V, S> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.map.table.is_none() {
            return None;
        }
        if self.entry.is_none() {
            let tab = self.map.table.as_ref().unwrap();
            if self.index >= tab.len() {
                return None;
            }
            while self.index < tab.len() {
                if let Some(ref e) = &tab[self.index] {
                    self.entry = Some(e);
                    self.index += 1;
                    break;
                }
                self.index += 1;
            }
        }

        if let Some(e) = self.entry {
            self.entry = e.next.as_ref();
            Some((&e.key, unsafe { e.value.as_ref().unwrap_unchecked() }))
        } else {
            None
        }
    }
}

pub struct VacantEntry<'a, K: Object, V: Object, S: 'static> {
    map: &'a mut Handle<HashMap<K, V, S>>,
    key: K,
    hash: u64,
}

impl<'a, K: 'static + Object, V: 'static + Object, S> VacantEntry<'a, K, V, S> {
    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn insert(self, value: V) -> &'a mut V
    where
        K: PartialEq + Eq,
        S: BuildHasher,
    {
        let thread = Thread::current();
        let mut tab = self.map.table.unwrap();
        let n = tab.len();
        let i = ((n as u64 - 1) & self.hash) as usize;

        let p = tab[i as usize];

        let node = thread.allocate(Node {
            hash: self.hash,
            key: self.key,
            value: Some(value),
            next: p,
        });

        thread.write_barrier(tab);
        tab[i] = Some(node);

        self.map.size += 1;
        self.map.mod_count += 1;
        if self.map.size > self.map.threshold {
            self.map.resize();
        }

        self.map.table.as_mut().unwrap()[i as usize]
            .as_mut()
            .unwrap()
            .value
            .as_mut()
            .unwrap()
    }
}

pub struct OccupiedEntry<'a, K: Object, V: Object, S: 'static> {
    map: &'a mut Handle<HashMap<K, V, S>>,
    node: Handle<Node<K, V>>,
}

impl<'a, K: 'static + Object, V: 'static + Object, S> OccupiedEntry<'a, K, V, S> {
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
        S: BuildHasher,
    {
        let old = self.node.value.take();
        self.map.remove_node(&self.node.key, self.node.hash);
        old.unwrap()
    }
}


pub enum Entry<'a, K: Object, V: Object, S: 'static> {
    Occupied(OccupiedEntry<'a, K, V, S>),
    Vacant(VacantEntry<'a, K, V, S>),
}