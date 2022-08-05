use rsgc::{
    memory::{
        traits::{Allocation, Finalize, ManagedObject, Trace},
        Heap,
    },
    Managed, weak_map::WeakMap,
};

struct Node {
    left: Option<Managed<Node>>,
    right: Option<Managed<Node>>,
}

unsafe impl Allocation for Node {}
unsafe impl Trace for Node {
    fn trace(&self, visitor: &mut dyn rsgc::memory::visitor::Visitor) {
        self.left.trace(visitor);
        self.right.trace(visitor);
    }
}
unsafe impl Finalize for Node {}
impl ManagedObject for Node {}

fn make_tree(heap: &mut Heap, depth: i32) -> Managed<Node> {
    if depth <= 0 {
        return heap.manage(Node {
            left: None,
            right: None,
        });
    }

    let left = make_tree(heap, depth - 1);
    let right = make_tree(heap, depth - 1);

    heap.manage(Node {
        left: Some(left),
        right: Some(right),
    })
}
#[cold]
#[inline(never)]
fn foo(heap: &mut Heap, mut map: Managed<WeakMap<i32>>) {
    let mut val = heap.manage(42);
    map.set(val, 43);
    val = heap.manage(1);
    map.set(val, 2);
}

fn main() {
    let mut heap = Heap::new();
    let mut weak_map = WeakMap::<i32>::new(&mut heap);

    foo(&mut heap, weak_map);   
    println!("{}", weak_map.len());
    heap.collect();
    println!("{}", weak_map.len());
}
