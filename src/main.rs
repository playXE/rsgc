use rsgc::{
    memory::{
        page::PageSpaceConfig,
        traits::{Allocation, Finalize, ManagedObject, Trace},
        Heap,
    },
    Managed,
};

pub struct Node {
    left: Option<Managed<Node>>,
    right: Option<Managed<Node>>,
    item: i32,
}

unsafe impl Trace for Node {
    fn trace(&self, visitor: &mut dyn rsgc::memory::visitor::Visitor) {
        self.left.trace(visitor);
        self.right.trace(visitor);
    }
}

unsafe impl Finalize for Node {}
unsafe impl Allocation for Node {}
impl ManagedObject for Node {}

fn bottom_up_tree(heap: &mut Heap, item: i32, mut depth: i32) -> Managed<Node> {
    if depth > 0 {
        let i = item + depth;
        depth -= 1;
        let left = bottom_up_tree(heap, i - 1, depth);
        let right = bottom_up_tree(heap, i, depth);

        make_node(heap, Some(left), Some(right), item)
    } else {
        make_node(heap, None, None, item)
    }
}

fn item_check(tree: Managed<Node>, i: usize) -> i32 {
    if let Some(left) = tree.left {
        return tree.item + item_check(left, i + 1) - item_check(tree.right.unwrap(), i + 1);
    } else {
        return tree.item;
    }
}
#[inline(never)]
pub fn make_node(
    heap: &mut Heap,
    left: Option<Managed<Node>>,
    right: Option<Managed<Node>>,
    item: i32,
) -> Managed<Node> {
    heap.manage(Node { left, right, item })
}

fn main() {
    let config = PageSpaceConfig::from_env();
    let mut heap = Heap::new(config);
    heap.add_core_roots();
    let min_depth = 4;
    let max_depth = min_depth + 8;
    {
        let stretch_depth = max_depth + 1;
        let stretch_tree = bottom_up_tree(&mut heap, 0, stretch_depth);

        println!(
            "stretch tree of depth {}  check {}",
            stretch_depth,
            item_check(stretch_tree, 0),
        );
    }

    let _long_lived_tree = bottom_up_tree(&mut heap, 0, max_depth);

    for depth in min_depth..max_depth {
        let iterations = 2i32.pow((max_depth - depth + min_depth) as u32);
        let mut check = 0;
        for _ in 0..iterations {
            check += item_check(bottom_up_tree(&mut heap, 1, depth), 0)
                + item_check(bottom_up_tree(&mut heap, -1, depth), 0);
        }

        println!(
            "{}\t trees of depth {}\t check: {}",
            iterations * 2,
            depth,
            check
        );
    }
    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        item_check(_long_lived_tree, 0)
    );
    heap.collect();
}
