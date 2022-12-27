use std::{collections::hash_map::RandomState, hash::Hash};

use rand::distributions::*;
use rsgc::{system::{collections::linked_hashmap::*, traits::Object, object::Handle, string::Str}, thread::{Thread, main_thread}, heap::region::HeapArguments};

struct Params {
    max_size: usize,
}

impl<K: Object, V: Object, S: 'static> LinkedHashMapExt<K, V, S> for Params {
    fn remove_eldest_entry(
        &self,
        map: &Handle<LinkedHashMap<K,V,S, Self>>,
        _eldest: &Handle<Node<K,V>>
    ) -> bool {
        println!("will remove? {} > {}", map.len(), self.max_size);
        map.len() > self.max_size
    }   
}

pub struct LRUCache<K: Object, V: Object> {
    hmap: Handle<LinkedHashMap<K, V, RandomState, Params>>
}

impl<K: 'static + Object + PartialEq + Eq + Hash, V: 'static + Object> LRUCache<K, V> {
    pub fn new(max_size: usize) -> Self {
        Self {
            hmap: LinkedHashMap::with_hasher_and_capacity(RandomState::new(), 16, Params { max_size }, true)
        }
    }

    pub fn get(&self, key: &K) -> Option<&V> 
    {
        self.hmap.get(key)
    }

    pub fn put(&mut self, thread: &mut Thread, key: K, value: V) {
        self.hmap.put(thread, key, value, true);
    }
}

pub struct CacheChurner {
    id: usize,
    size: usize,
    cache: LRUCache<usize, Handle<Str>>,
    hits: usize,
    misses: usize,
    target_hit_ratio: f64,
}

impl CacheChurner {
    pub fn new(id: usize, size: usize, target_hit_ratio: f64) -> Self {
        Self {
            id,
            size,
            cache: LRUCache::new(size),
            hits: 0,
            misses: 0,
            target_hit_ratio,
        }
    }

    pub fn run(&mut self) {
        let mut have_reported_full = false;
       
        loop {
            self.churn();
            if !have_reported_full && self.cache.hmap.len() == self.size {
                eprintln!("#{}: cache is now full", self.id);
                have_reported_full = true;
            }
        }

        
    }

    pub fn churn(&mut self) {
        let th = Thread::current(); println!("0..{}", self.size as f64 / self.target_hit_ratio as f64);
        for _ in 0..1000 {
            th.safepoint();
            let n = self.size as f64 / self.target_hit_ratio;
            let key = Uniform::new(0, n as usize).sample(&mut rand::thread_rng());
            if self.cache.get(&key).is_some() {
                
                self.hits += 1;
            } else {
                self.misses += 1;
                let s = Str::new(th, "value");
                self.cache.put(th, key, s);
                assert!(self.cache.hmap.len() <= self.size + 1, "too much keys? {}", self.cache.hmap.len());
            }
        }
    }
}

fn test_eviction() {
    let mut c = LRUCache::new(2);
    let t = Thread::current();
    let k = Str::new(t, "k1");
    let v = Str::new(t, "v1");
    let k1 = k;
    c.put(t, k, v);

    let k = Str::new(t, "k2");
    let v = Str::new(t, "v2");
    c.put(t, k, v);

    c.get(&k1).unwrap();

    let k = Str::new(t, "k3");
    let v = Str::new(t, "v3");
    c.put(t, k, v);

    assert_eq!(&*c.get(&k1).copied().unwrap(), "v1");
}

fn main() {
    env_logger::init();
    let args = HeapArguments::from_env();

    let _ = main_thread(args, |_| {

        test_eviction();
        let mut churner = CacheChurner::new(0, 10, 0.5);

        let mut i = 0;

        while i < 10000 {
            churner.churn();
            i += 1;
        }

        let hit_ratio = churner.hits as f64 / (churner.hits as f64 + churner.misses as f64);

        println!("hit ratio: {:.2}", hit_ratio);
        Ok(())
    }); 
}
