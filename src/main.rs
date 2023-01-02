use std::collections::hash_map::RandomState;

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
}