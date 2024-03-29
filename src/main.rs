use rsgc::{
    heap::{region::HeapArguments, thread::Thread},
    system::object::{Allocation, Handle},
    system::traits::Object,
};

#[allow(dead_code)]
pub struct TreeNode {
    item: i64,
    left: Option<Handle<Self>>,
    right: Option<Handle<Self>>,
}


unsafe impl Object for TreeNode {
    fn trace(&self, visitor: &mut dyn rsgc::system::traits::Visitor) {
        if let Some(ref left) = self.left {
            left.trace(visitor);
        }

        if let Some(ref right) = self.right {
            right.trace(visitor);
        }
    }
}

unsafe impl Allocation for TreeNode {
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
    thread.safepoint();
    let node = if 0 < depth {
        let mut node = thread.allocate(TreeNode {
            item: 0,
            left: None,
            right: None,
        });

        let left = create_tree(thread, depth - 1);
        let right = create_tree(thread, depth - 1);

        thread.write_barrier(node);
        node.left = Some(left);
        node.right = Some(right);

        node 
    } else {
        let node = TreeNode {
            item: 0,
            left: None,
            right: None,
        };

        thread.allocate(node)
    };
    
    
    node
}


fn bench() {
    let mut n = 0;
    if let Some(arg) = std::env::args().skip(1).next() {
        if let Ok(x) = arg.parse::<usize>() {
            n = x;
        }
    }

    let min_depth = 4;
    let max_depth = if n < (min_depth + 2) {
        min_depth + 2
    } else {
        n 
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
    let thread = Thread::current();

    let mut d = min_depth;

    while d <= max_depth {
        let depth = d;

        let iterations = 1 << (max_depth - depth + min_depth);

        let mut check = 0;

        for _ in 1..=iterations {
            
            let tree_node = create_tree(thread, depth as _);
            
            check += tree_node.as_ref().check_tree();

        }

        println!("{}\t trees of depth {}\t check: {}", iterations, depth, check);

        d += 2;
    }

    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        long_lasting_tree.as_ref().check_tree()
    );

    println!(
        "time: {}ms",
        start.elapsed().as_millis()
    );
}

fn main() {
    env_logger::init();
    let args = HeapArguments::from_env();

    let _ = rsgc::thread::main_thread(args, |heap| {
        heap.add_core_root_set();

        bench();

        Ok(())
    });
}