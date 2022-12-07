use std::{sync::atomic::AtomicUsize, mem::size_of};

use rsgc::{
    heap::{
        heap::Heap,
        region::HeapArguments,
        thread::{self, ThreadInfo},
    },
    object::{Allocation, Array, Handle, HeapObjectHeader},
    traits::Object, formatted_size,
};

struct Node {
    right: Option<Handle<Node>>,
    left: Option<Handle<Node>>,
    val: i32,
}

impl Object for Node {
    fn trace(&self, visitor: &mut dyn rsgc::traits::Visitor) {
        if let Some(ref next) = self.right {
            next.trace(visitor);
        }

        if let Some(ref next) = self.left {
            next.trace(visitor);
        }
    }
}

impl Allocation for Node {}

fn bottom_up_tree(thread: &mut ThreadInfo, depth: i32) -> Handle<Node> {
   thread.safepoint();
    if depth <= 0 {
        return thread.allocate_fixed(Node {
            right: None,
            left: None,
            val: 0,
        });
    }

    let left = bottom_up_tree(thread, depth - 1);
    let right = bottom_up_tree(thread, depth - 1);

    thread.allocate_fixed(Node {
        left: Some(left),
        right: Some(right),
        val: depth,
    })
}

fn main() {
    env_logger::init();

    let mut args = HeapArguments::default();
    args.region_size = 16 * 1024;
    let heap = Heap::new(args);

    let tree = bottom_up_tree(thread::thread(), 18);
    heap.request_gc();
    unsafe {
        heap.stop();
    }
}
