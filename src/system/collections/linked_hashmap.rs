//! LinkedHashMap implementation based on OpenJDK one
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
    pub(super) before: Option<Handle<Node<K, V>>>,
    pub(super) after: Option<Handle<Node<K, V>>>,
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
            before: None,
            after: None,
        }
    }

    pub fn set_value(&mut self, value: V) {
        self.value = Some(value);
    }

    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn value(&self) -> &V {
        unsafe { self.value.as_ref().unwrap_unchecked() }
    }

    pub fn next(&self) -> Option<&Handle<Node<K, V>>> {
        self.next.as_ref()
    }

    pub unsafe fn set_next(&mut self, next: Option<Handle<Node<K, V>>>) {
        self.next = next;
    }

    pub fn after(&self) -> Option<&Handle<Node<K, V>>> {
        self.after.as_ref()
    }

    pub fn before(&self) -> Option<&Handle<Node<K, V>>> {
        self.before.as_ref()
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

unsafe impl<K: Object, V: Object> Object for Node<K, V> {
    fn trace(&self, visitor: &mut dyn Visitor) {
        self.key.trace(visitor);
        self.value.trace(visitor);
        self.before.trace(visitor);
        self.after.trace(visitor);
    }
}

unsafe impl<K: Object, V: Object> Allocation for Node<K, V> {}

pub enum DefaultHashBuilder {}

pub trait LinkedHashMapExt<K: Object, V: Object, S: 'static>: 'static + Sized {
    fn remove_eldest_entry(
        &self,
        map: &Handle<LinkedHashMap<K, V, S, Self>>,
        eldest: &Handle<Node<K, V>>,
    ) -> bool {
        let _ = map;
        let _ = eldest;
        false
    }
}

pub struct DefaultLinkedHashMapExt;

impl Default for DefaultLinkedHashMapExt {
    fn default() -> Self {
        Self {}
    }
}

impl<K: Object, V: Object, S: 'static> LinkedHashMapExt<K, V, S> for DefaultLinkedHashMapExt {}

pub struct LinkedHashMap<
    K: Object,
    V: Object,
    S: 'static = RandomState,
    E: LinkedHashMapExt<K, V, S> = DefaultLinkedHashMapExt,
> {
    table: Option<Handle<Array<Option<Handle<Node<K, V>>>>>>,
    head: Option<Handle<Node<K, V>>>,
    tail: Option<Handle<Node<K, V>>>,
    size: u32,
    mod_count: u32,
    threshold: u32,
    load_factor: f32,
    access_order: bool,
    marker: PhantomData<*const Node<K, V>>,
    ext: E,
    hash_builder: S,
}

impl<
        K: 'static + Object,
        V: 'static + Object,
        S: 'static + BuildHasher,
        E: LinkedHashMapExt<K, V, S>,
    > LinkedHashMap<K, V, S, E>
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

    pub fn with_hasher(hash_builder: S, ext: E, access_order: bool) -> Handle<Self> {
        Self::with_hasher_and_capacity(
            hash_builder,
            Self::DEFAULT_INITIAL_CAPACITY,
            ext,
            access_order,
        )
    }

    pub fn with_hasher_and_capacity(
        hash_builder: S,
        initial_capacity: u32,
        ext: E,
        access_order: bool,
    ) -> Handle<Self> {
        Self::with_hasher_and_capacity_and_load_factor(
            hash_builder,
            initial_capacity,
            Self::DEFAULT_LOAD_FACTOR,
            ext,
            access_order,
        )
    }

    pub fn with_hasher_and_capacity_and_load_factor(
        hash_builder: S,
        mut initial_capacity: u32,
        load_factor: f32,
        ext: E,
        access_order: bool,
    ) -> Handle<Self> {
        if initial_capacity > Self::MAXIMUM_CAPACITY {
            initial_capacity = Self::MAXIMUM_CAPACITY;
        }
        if load_factor <= 0.0 || load_factor.is_nan() {
            panic!("Illegal load factor");
        }

        Thread::current().allocate(Self {
            table: None,
            head: None,
            tail: None,
            size: 0,
            mod_count: 0,
            threshold: Self::table_size_for(initial_capacity as _) as _,
            load_factor,
            ext,
            marker: PhantomData,
            hash_builder,
            access_order,
        })
    }

    fn link_node_last(self: &mut Handle<Self>, thread: &mut Thread, mut p: Handle<Node<K, V>>) {
        let last = self.tail;
        thread.write_barrier(*self);
        self.tail = Some(p);

        if let Some(mut last) = last {
            thread.write_barrier(last);
            thread.write_barrier(p);
            last.after = Some(p);
            p.before = Some(last);
        } else {
            self.head = Some(p);
        }
    }

    fn transfer_links(
        self: &mut Handle<Self>,
        thread: &mut Thread,
        src: Handle<Node<K, V>>,
        mut dst: Handle<Node<K, V>>,
    ) {
        thread.write_barrier(dst);
        dst.before = src.before;
        dst.after = src.after;

        let b = dst.before;
        let a = dst.after;

        if let Some(mut b) = b {
            thread.write_barrier(b);
            b.after = Some(dst);
        } else {
            thread.write_barrier(*self);
            self.head = Some(dst);
        }

        if let Some(mut a) = a {
            thread.write_barrier(a);
            a.before = Some(dst);
        } else {
            thread.write_barrier(*self);
            self.tail = Some(dst);
        }
    }

    fn make_node(
        self: &mut Handle<Self>,
        thread: &mut Thread,
        hash: u64,
        key: K,
        value: V,
        e: Option<Handle<Node<K, V>>>,
    ) -> Handle<Node<K, V>> {
        let p = thread.allocate(Node {
            hash,
            key,
            value: Some(value),
            before: None,
            after: None,
            next: e,
        });

        self.link_node_last(thread, p);

        p
    }

    fn after_node_removal(self: &mut Handle<Self>, thread: &mut Thread, mut e: Handle<Node<K, V>>) {
        let b = e.before;
        let a = e.after;

        if let Some(mut b) = b {
            thread.write_barrier(b);
            b.after = a;
        } else {
            thread.write_barrier(*self);
            self.head = a;
        }

        if let Some(mut a) = a {
            thread.write_barrier(a);
            a.before = b;
        } else {
            thread.write_barrier(*self);
            self.tail = b;
        }

        thread.write_barrier(e);
        e.before = None;
        e.after = None;
    }

    fn after_node_insertion(self: &mut Handle<Self>, evict: bool)
    where
        K: Hash,
    {
        let first = self.head;

        if let Some(first) = first {
            if evict && self.ext.remove_eldest_entry(self, &first) {
                let hash = make_hash(&self.hash_builder, &first.key);
                self.remove_node(&first.key, hash);
            }
        }
    }

    fn after_node_access(self: &mut Handle<Self>, thread: &mut Thread, e: Handle<Node<K, V>>)
    where
        K: Hash,
    {
        let last = self.tail;
        if last.map(|x| !Handle::ptr_eq(&x, &e)).unwrap_or(true) {
            if self.access_order {
                let mut p = e;
                let b = p.before;
                let a = p.after;

                p.after = None;

                if let Some(mut b) = b {
                    thread.write_barrier(b);
                    b.after = a;
                } else {
                    thread.write_barrier(*self);
                    self.head = a;
                }

                if let Some(mut a) = a {
                    thread.write_barrier(a);
                    a.before = b;
                } else {
                    thread.write_barrier(*self);
                    self.tail = b;
                }

                if let Some(mut last) = last {
                    thread.write_barrier(p);
                    thread.write_barrier(last);
                    p.before = Some(last);
                    last.after = Some(p);
                } else {
                    thread.write_barrier(p);
                    self.head = Some(p);
                }
                self.tail = Some(p);
            }
        }
    }

    pub fn capacity_from_threshold(&self) -> u32 {
        if self.threshold == 0 {
            0
        } else {
            (self.threshold as usize * 2).next_power_of_two() as u32
        }
    }

    fn get_node<Q>(&self, key: &Q) -> Option<&Handle<Node<K, V>>>
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

    pub fn get<'a, Q>(self: &'a Handle<Self>, key: &Q) -> Option<&'a V>
    where
        K: Borrow<Q> + Hash,
        Q: Hash + Eq + ?Sized,
    {
        match self.get_node(key) {
            None => None,
            Some(node) => {
                let mut this = *self;
                if self.access_order {
                    this.after_node_access(Thread::current(), *node);
                }
                Some(node.value.as_ref().unwrap())
            }
        }
    }

    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_node(key).is_some()
    }

    pub fn put<'a>(
        self: &'a mut Handle<Self>,
        thread: &mut Thread,
        key: K,
        value: V,
        evict: bool,
    ) -> Option<V>
    where
        K: Hash,
    {
        let hash = make_hash(&self.hash_builder, &key);
        self.put_val(thread, hash, key, value, evict)
    }

    fn put_val<'a>(
        self: &'a mut Handle<Self>,
        thread: &mut Thread,
        hash: u64,
        key: K,
        value: V,
        evict: bool,
    ) -> Option<V>
    where
        K: Hash,
    {
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
                self.after_node_access(thread, n);
                return n.value.replace(value);
            }
            node = n.next
        }

        let node = self.make_node(thread, hash, key, value, tab[i]);
        tab[i] = Some(node);

        self.mod_count += 1;
        self.size += 1;
        if self.size > self.threshold {
            self.resize();
        }
        self.after_node_insertion(evict);

        None
    }

    fn resize(self: &mut Handle<Self>) -> Option<Handle<Array<Option<Handle<Node<K, V>>>>>> {
        let thread = Thread::current();

        let old_tab = self.table;
        let old_cap = old_tab.as_ref().map(|tab| tab.len()).unwrap_or(0) as u32;
        let mut new_cap = 0;
        let mut new_thr = 0;
        let old_thr = self.threshold;
        if old_cap > 0 {
            if old_cap >= Self::MAXIMUM_CAPACITY {
                self.threshold = std::u32::MAX;
                return old_tab;
            } else if ((old_cap << 1) as u32) < Self::MAXIMUM_CAPACITY
                && old_cap >= Self::DEFAULT_INITIAL_CAPACITY
            {
                new_cap = old_cap << 1;
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
                    println!("removed!");
                    if Handle::ptr_eq(&node, &p) {
                        thread.write_barrier(tab);
                        tab[i] = node.next;
                    } else {
                        thread.write_barrier(p);
                        p.next = node.next;
                    }
                    self.mod_count += 1;
                    self.size -= 1;
                    self.after_node_removal(thread, node);
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

        self.head = None;
        self.tail = None;
    }

    pub fn front<'a>(self: &'a Handle<Self>) -> Option<(&'a K, &'a V)> {
        self.head.as_ref().map(|head| unsafe {
            let key = &head.key;
            let value = head.value.as_ref().unwrap_unchecked();
            (key, value)
        })
    }

    pub fn back<'a>(self: &'a Handle<Self>) -> Option<(&'a K, &'a V)> {
        self.tail.as_ref().map(|tail| unsafe {
            let key = &tail.key;
            let value = tail.value.as_ref().unwrap_unchecked();
            (key, value)
        })
    }

    pub fn pop_front(self: &mut Handle<Self>) -> Option<Handle<Node<K, V>>>
    where
        K: Hash,
    {
        if let Some(head) = self.head {
            self.remove_node(&head.key, head.hash);
            return Some(head);
        }

        None
    }

    pub fn pop_back(self: &mut Handle<Self>) -> Option<Handle<Node<K, V>>>
    where
        K: Hash,
    {
        if let Some(tail) = self.tail {
            self.remove_node(&tail.key, tail.hash);
            return Some(tail);
        }

        None
    }
}

impl<K: Object, V: Object, S: 'static, E: LinkedHashMapExt<K, V, S>> LinkedHashMap<K, V, S, E> {
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

    pub fn iter<'a>(self: &'a Handle<Self>) -> Iter<'a, K, V, S, E> {
        Iter {
            head: self.head.as_ref(),
            map: self,
            index: 0,
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

unsafe impl<K: Object, V: Object, S: 'static, E: LinkedHashMapExt<K, V, S>> Object
    for LinkedHashMap<K, V, S, E>
{
    fn trace(&self, visitor: &mut dyn Visitor) {
        self.table.trace(visitor);
    }
}

unsafe impl<K: Object, V: Object, S: 'static, E: LinkedHashMapExt<K, V, S>> Allocation
    for LinkedHashMap<K, V, S, E>
{
}

pub struct Iter<'a, K: Object, V: Object, S: 'static, E: LinkedHashMapExt<K, V, S>> {
    map: &'a LinkedHashMap<K, V, S, E>,
    index: usize,
    head: Option<&'a Handle<Node<K, V>>>,
}

impl<'a, K: Object, V: Object, S: 'static, E: LinkedHashMapExt<K, V, S>> Iterator
    for Iter<'a, K, V, S, E>
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.map.table.is_none() {
            return None;
        }
        match self.head {
            Some(head) => {
                self.head = head.after.as_ref();
                Some(unsafe { (&head.key, &head.value.as_ref().unwrap_unchecked()) })
            }
            None => None,
        }
    }
}
