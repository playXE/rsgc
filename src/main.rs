/*use std::collections::hash_map::RandomState;

use rsgc::{prelude::*, system::collections::hashmap::*};

fn main() {
    let _ = main_thread(HeapArguments::from_env(), |_| {
        let mut map = HashMap::with_hasher_and_capacity(RandomState::new(), 16);

        let entry = map.entry(42);

        match entry {
            Entry::Occupied(_) => {
                unreachable!()
            }
            Entry::Vacant(x) => {
                x.insert(43);
            }
        }

        let entry = map.entry(42);
        match entry {
            Entry::Occupied(x) => {
                assert_eq!(x.get(), &43);
            }
            Entry::Vacant(_) => {
                unreachable!()
            }
        }
        Ok(())
    });
}*/

use std::{time::Instant, sync::atomic::AtomicUsize};

static X: AtomicUsize = AtomicUsize::new(2000);

fn main() {
    for _ in 0..100 {
        let start = Instant::now();
        let x = std::hint::black_box(|| {
            let x = Instant::now();
            for i in std::hint::black_box(0..X.load(atomic::Ordering::Relaxed)) {
                let _ = std::hint::black_box(i * i);
            }
            x.elapsed()
        })();
        let end = start.elapsed();

        println!("{}ns ({}ns)", end.as_nanos(), x.as_nanos());
    }
}