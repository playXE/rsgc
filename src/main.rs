use std::sync::Arc;

use rsgc::{
    env::read_uint_from_env,
    heap::{region::HeapArguments, thread::Thread},
    keep_on_stack,
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
    const DESTRUCTIBLE: bool = false;
}

impl TreeNode {
    fn check_tree(&self) -> usize {
        if self.left.is_none() {
            return 1;
        }

        1 + self.left.unwrap().check_tree() + self.right.unwrap().check_tree()
    }
}

fn create_tree(thread: &mut Thread, depth: i64) -> Handle<TreeNode> {
    if 0 < depth {
        let left = create_tree(thread, depth - 1);
        let right = create_tree(thread, depth - 1);

        let node = TreeNode {
            item: 0,
            left: Some(left),
            right: Some(right),
        };

        keep_on_stack(&node);

        thread.allocate_fixed(node)
    } else {
        let node = TreeNode {
            item: 0,
            left: None,
            right: None,
        };

        keep_on_stack(&node);

        thread.allocate_fixed(node)
    }
}

fn bench_parallel() {
    let min_depth = 4;
    let max_depth = match read_uint_from_env("TREE_DEPTH") {
        Some(x) if x < min_depth + 2 => min_depth + 2,
        Some(x) => x,
        _ => 21,
    };

    let start = std::time::Instant::now();
    let stretch_depth = max_depth + 1;

    {
        println!(
            "stretch tree of depth {}\t check: {}",
            stretch_depth,
            create_tree(Thread::current(), stretch_depth as _)
                .as_ref()
                .check_tree()
        );
    }

    let long_lasting_tree = create_tree(Thread::current(), max_depth as _);
    use parking_lot::Mutex;
    let results = Arc::new(
        (0..(max_depth - min_depth) / 2 + 1)
            .map(|_| Mutex::new(String::new()))
            .collect::<Vec<_>>(),
    );
    //let safe_scope = SafeScope::new(rsgc::heap::thread::thread());
    //std::thread::scope(|scope| {
    let mut d = min_depth;

    while d <= max_depth {
        let depth = d;
        let cloned = results.clone();
        //scope.spawn(move || {
        let thread = Thread::current();
        let iterations = 1 << (max_depth - depth + min_depth);
        let mut check = 0;
        for _ in 1..=iterations {
            let tree_node = create_tree(thread, depth as _);
            check += tree_node.as_ref().check_tree();
        }

        *cloned[(depth - min_depth) / 2].lock() = format!(
            "{}\t trees of depth {}\t check: {}",
            iterations, depth, check
        );
        //});

        d += 2;
    }

    //});
    //drop(safe_scope);
    for result in results.iter() {
        println!("{}", *result.lock());
    }
    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        long_lasting_tree.as_ref().check_tree()
    );

    println!(
        "binary trees took: {:03} secs",
        start.elapsed().as_micros() as f64 / 1000.0 / 1000.0
    );
}

fn main() {
    env_logger::init();
    let args = HeapArguments::from_env();

    rsgc::thread::main_thread(args, |_| {
        bench_parallel();
    });
}
