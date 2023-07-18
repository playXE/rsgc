use std::sync::{atomic::AtomicBool, Arc, Barrier};

use immix::{heap::Heap, thread::{manage_thread, Thread, unmanage_thread}, traits::{Object, Allocation, Visitor}, object_header::Handle};


#[allow(dead_code)]
pub struct TreeNode {
    item: i64,
    left: Option<Handle<Self>>,
    right: Option<Handle<Self>>,
}


unsafe impl Object for TreeNode {
    fn trace(&self, visitor: &mut dyn Visitor) {
        if let Some(ref left) = self.left {
            left.trace(visitor);
        }

        if let Some(ref right) = self.right {
            right.trace(visitor);
        }
    }
}

unsafe impl Allocation for TreeNode {
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
    let node = if 0 < depth {
        let mut node = thread.allocate(TreeNode {
            item: 0,
            left: None,
            right: None,
        });

        let left = create_tree(thread, depth - 1);
        let right = create_tree(thread, depth - 1);

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
static GCED: AtomicBool= AtomicBool::new(false);

fn main() {
    env_logger::init();
    Heap::init(256 * 1024 * 1024, 4 * 1024 * 1024 * 1024);

    manage_thread();
    let b1 = Arc::new(Barrier::new(2));
    let b2 = b1.clone();
    let handle = std::thread::spawn(move || {
        manage_thread();

        b2.wait();
        let thread = Thread::current();

        while !GCED.load(std::sync::atomic::Ordering::Relaxed) {
            thread.safepoint();
        }
        println!("lol!");

        unmanage_thread();
    });
    b1.wait();
    Heap::get().collect();
    GCED.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.join().unwrap();
    unmanage_thread();
}