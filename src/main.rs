use rsgc::{
    env::read_uint_from_env,
    heap::{heap::Heap, region::HeapArguments, thread::ThreadInfo},
    object::{Allocation, Handle},
    traits::Object,
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

impl Allocation for TreeNode {
    const FINALIZE: bool = false;
    const LIGHT_FINALIZER: bool = false;
}

impl TreeNode {
    fn check_tree(&self) -> usize {
        match (self.left, self.right) {
            (Some(left), Some(right)) => {
                left.as_ref().check_tree() + right.as_ref().check_tree() + 1
            }
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
        thread.write_barrier(node);
        node.as_mut().right = Some(create_tree(thread, depth));
        thread.write_barrier(node);
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

    println!(
        "{}\t trees of depth {}\t check: {}",
        iterations, depth, check
    );
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

    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        long_lasting_tree.as_ref().check_tree()
    );
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
        println!(
            "stretch tree of depth {}\t check: {}",
            stretch_depth,
            create_tree(rsgc::heap::thread::thread(), stretch_depth as _)
                .as_ref()
                .check_tree()
        );
    }

    trees(max_depth as _);

    println!(
        "binary trees took: {} secs",
        start.elapsed().as_micros() as f64 / 1000.0 / 1000.0
    );
}

struct List {
    next: Option<Handle<List>>,
    val: usize
}

impl Object for List {
    fn trace(&self, visitor: &mut dyn rsgc::traits::Visitor) {
        match self.next {
            Some(ref obj) => obj.trace(visitor),
            _ => ()
        }
    }
}

impl Allocation for List {}

fn main() {
    env_logger::init();
    let args = HeapArguments::from_env();
    let _ = Heap::new(args);
    /* 
    let thread = rsgc::heap::thread::thread();
    let mut list =  thread.allocate_fixed(List {
        next: None,
        val: 0
    });

    for i in 0..16 * 1024 * 1024 * 100 {
        if i % 10 * 1024 == 0 {
            list = thread.allocate_fixed(List {
                next: None,
                val: i
            });
        } else {
            let mut new = thread.allocate_fixed(List {
                next: None,
                val: i
            });
            new.as_mut().next = Some(list);
            thread.write_barrier(new);
            list = new;
        }
    }*/

    bench();
}
