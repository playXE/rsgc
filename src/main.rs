use rsgc::heap::thread;
use rsgc::{
    heap::{thread::ThreadInfo, region::HeapArguments, heap::{Heap, heap}},
    object::{Allocation, Handle},
    traits::Object, env::read_uint_from_env,
};

#[allow(dead_code)]
pub struct TreeNode {
    item: i64,
    left: Option<Handle<Self>>,
    right: Option<Handle<Self>>,
}


impl Object for TreeNode {
    fn trace(&self, visitor: &mut dyn rsgc::traits::Visitor) {
        if let Some(ref left) = self.left {
            left.trace(visitor);
        }

        if let Some(ref right) = self.right {
            right.trace(visitor);
        }
    }
}

impl Allocation for TreeNode {}

impl TreeNode {
    fn check_tree(&self) -> usize {
        match (self.left, self.right) {
            (Some(left), Some(right)) => left.as_ref().check_tree() + right.as_ref().check_tree() + 1,
            _ => 1,
        }
    }
}

fn create_tree(thread: &mut ThreadInfo, mut depth: i64) -> Handle<TreeNode> {
    thread.safepoint();
    let mut node = thread.allocate_fixed(TreeNode {
        left: None,
        right: None,
        item: depth,
    });

    if depth > 0 {
        depth -= 1;

        node.as_mut().left = Some(create_tree(thread, depth));
        node.as_mut().right = Some(create_tree(thread, depth));
    }

    node
}

fn loops(iterations: i64, depth: i64) {
    let mut check = 0;
    let mut item = 0;
    let th = rsgc::heap::thread::thread();
    while {
        th.safepoint();
        check += create_tree(th, depth).as_ref().check_tree();
        item += 1;
        item < iterations
    } {}

    println!("{}\t trees of depth {}\t check: {}", iterations, depth, check);
}

fn trees(max_depth: i64) {
    let long_lasting_tree = create_tree(rsgc::heap::thread::thread(), max_depth);

    let mut depth = 4;

    while {
        let iterations = 16 << (max_depth - depth);
        loops(iterations, depth);
        depth += 2;
        depth <= max_depth
    } {}

    println!("long lived tree of depth {}\t check: {}", max_depth, long_lasting_tree.as_ref().check_tree());
}

#[inline(never)]
#[cold]
fn bench() {
    let max_depth = match read_uint_from_env("TREE_DEPTH") {
        Some(x) if x > 6 => x,
        _ => 6,
    };

    let start = std::time::Instant::now();
    let stretch_depth = max_depth + 1;

    {
        println!("stretch tree of depth {}\t check: {}", stretch_depth, create_tree(rsgc::heap::thread::thread(), stretch_depth as _).as_ref().check_tree());
    }

    trees(max_depth as _);

    println!("binary trees took: {} secs", start.elapsed().as_micros() as f64 / 1000.0 / 1000.0);
}

fn main() {
    env_logger::init();
    let mut args = HeapArguments::from_env();
    args.target_num_regions = 2048;
    args.parallel_region_stride = 4;
    args.allocation_threshold = 10;
    args.parallel_gc_threads = 4;
    let _ = Heap::new(args);
    let mut handles = vec![];
    for _ in 0..6 {
        handles.push(thread::spawn(|| {
            bench();
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}