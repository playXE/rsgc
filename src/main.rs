use std::{mem::size_of, sync::atomic::{AtomicUsize, AtomicBool, Ordering}};

use rsgc::{
    force_on_stack, formatted_size,
    heap::{
        heap::{heap, Heap},
        region::HeapArguments,
        thread::{self, ThreadInfo},
    },
    object::{Allocation, Array, Handle, HeapObjectHeader},
    traits::Object,
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

        /*if PRINT_TRACE.load(Ordering::Relaxed) {
            println!("TRACE NODE {:p} {}", self, self.val);
        }*/
    }
}

impl Allocation for Node {}

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);

fn bottom_up_tree(thread: &mut ThreadInfo, depth: i32) -> Handle<Node> {
    thread.safepoint();
    //ALLOCATED.fetch_add(size_of::<Node>() + 8, std::sync::atomic::Ordering::Relaxed);
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

#[inline(never)]
#[cold]
fn tree() {
    let _tree = bottom_up_tree(thread::thread(), 25);

    //force_on_stack(&_tree);
}

#[inline(never)]
#[cold]
fn list(n: usize) {
    let thread = thread::thread();
    let mut head = thread.allocate_fixed(List {
        next: None,
        value: 0
    });

    for i in 0..n {
        if i % 16 * 1024 == 0 {
            thread.safepoint();
            head = thread.allocate_fixed(List {
                next: None,
                value: 0
            });
        } else {
            let next = thread.allocate_fixed(List {
                next: Some(head),
                value: head.as_ref().value + 1
            }); 
            head = next;
        }
    }
}


struct List {
    next: Option<Handle<List>>,
    value: i64,
}

impl Allocation for List {}

impl Object for List {
    fn trace(&self, visitor: &mut dyn rsgc::traits::Visitor) {
        if let Some(ref next) = self.next {
            next.trace(visitor);
        }

        if PRINT_TRACE.load(Ordering::Relaxed) {
            println!("TRACE {:p} {}", self, self.value);
        }
    }
}

static PRINT_TRACE: AtomicBool = AtomicBool::new(false);

fn main() {
    env_logger::init();
    

    let mut args = HeapArguments::default();
    args.max_heap_size = 2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024;
    args.parallel_region_stride = 256;
    args.parallel_gc_threads = 4;
    args.allocation_threshold = 20;
    args.min_free_threshold = 10;
    args.target_num_regions = 2048;
    args.tlab_size = 4 * 1024;
    let _ = Heap::new(args);
    let time = std::time::Instant::now();
    tree();
    println!("bottom up tree in {:04} msecs", time.elapsed().as_micros() as f64 / 1000.0);

    //PRINT_TRACE.store(true, Ordering::Relaxed);
    for _ in 0..2 {
        heap().request_gc();
       
    }
    unsafe {
        heap().stop();
    }

    println!(
        "allocated {}",
        formatted_size(ALLOCATED.load(std::sync::atomic::Ordering::Relaxed))
    );
}
