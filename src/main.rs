use std::{iter, sync::Arc};

use rsgc::{
    env::read_uint_from_env,
    heap::{heap::Heap, region::HeapArguments, thread::{Thread, SafeScope}, GCHeuristic},
    object::{Allocation, Handle, Array},
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
        match (self.left, self.right) {
            (Some(left), Some(right)) => {
                left.as_ref().check_tree() + right.as_ref().check_tree() + 1
            }
            _ => 1,
        }
    }
}

fn create_tree(thread: &mut Thread, mut depth: i64) -> Handle<TreeNode> {
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
    let th = Thread::current();
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
    let mut depth = 4;

    while {
        let iterations = 16 << (max_depth - depth);
        loops(iterations, depth);
        depth += 2;
        depth <= max_depth
    } {}

}

#[inline(never)]
#[cold]
fn bench() {
    let max_depth = match read_uint_from_env("TREE_DEPTH") {
        Some(x) if x > 6 => x,
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
    trees(max_depth as _);

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


/* 
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
            create_tree(rsgc::heap::thread::thread(), stretch_depth as _)
                .as_ref()
                .check_tree()
        );
    }



    let long_lasting_tree = create_tree(rsgc::heap::thread::thread(), max_depth as _);
    use parking_lot::Mutex;
    let results =  Arc::new((0..(max_depth - min_depth) / 2 + 1).map(|_| Mutex::new(String::new())).collect::<Vec<_>>());
    let safe_scope = SafeScope::new(rsgc::heap::thread::thread());
    std::thread::scope(|scope| {
        let mut d = min_depth;
       
        while d <= max_depth {
            let depth = d;
            let cloned = results.clone();
            scope.spawn(move || {
                let thread = rsgc::heap::thread::thread();
                let iterations = 1 << (max_depth - depth + min_depth);
                let mut check = 0;
                for _ in 1..iterations {    
                    let tree_node = create_tree(thread, depth as _);
                    check += tree_node.as_ref().check_tree();
                }
                
                
                *cloned[(depth - min_depth) / 2].lock() = format!("{}\t trees of depth {}\t check: {}", iterations, depth, check);
            });

            d += 2;   
        }

        
    });
    drop(safe_scope);
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
}*/

fn main() {
    env_logger::init();
    let args = HeapArguments::from_env();
    rsgc::thread::main_thread(args, |_| {
        bench();
    });
}
