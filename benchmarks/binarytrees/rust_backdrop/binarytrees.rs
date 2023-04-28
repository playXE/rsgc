
use backdrop_arc::TrashThreadStrategy;
use backdrop_arc::Arc;
use snmalloc_rs::SnMalloc;

#[global_allocator]
static ALLOC: SnMalloc = SnMalloc;

#[allow(dead_code)]
pub struct Node {
    item: i64,
    left: Option<Arc<Node, TrashThreadStrategy>>,
    right: Option<Arc<Node, TrashThreadStrategy>>,
}

impl Node {
    fn check_tree(&self) -> usize {
        if self.left.is_none() {
            return 1;
        }

        1 + self.left.as_ref().unwrap().check_tree() + self.right.as_ref().unwrap().check_tree()
    }
}

fn create_tree(depth: i64) -> Arc<Node, TrashThreadStrategy> {
    if 0 < depth {
        let node = Arc::new(Node {
            item: 0,
            left: Some(create_tree(depth - 1)),
            right: Some(create_tree(depth - 1)),
        });

        node
    } else {
        let node = Node {
            item: 0,
            left: None,
            right: None,
        };

        Arc::new(node)
    }
}

fn bench_parallel() {
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
            create_tree(stretch_depth as _).check_tree()
        );
    }

    let long_lasting_tree = create_tree(max_depth as _);
    use parking_lot::Mutex;
    let results: Arc<_, TrashThreadStrategy> = Arc::new(
        (0..(max_depth - min_depth) / 2 + 1)
            .map(|_| Mutex::new(String::new()))
            .collect::<Vec<_>>(),
    );

    std::thread::scope(|scope| {
        let mut d = min_depth;

        while d <= max_depth {
            let depth = d;
            let cloned = Arc::clone(&results);
            scope.spawn(move || {
                let iterations = 1 << (max_depth - depth + min_depth);
                let mut check = 0;
                for _ in 1..=iterations {
                    let tree_node = create_tree(depth as _);
                    check += tree_node.check_tree();
                }

                *cloned[(depth - min_depth) / 2].lock() = format!(
                    "{}\t trees of depth {}\t check: {}",
                    iterations, depth, check
                );
            });

            d += 2;
        }
    });

    for result in results.iter() {
        println!("{}", *result.lock());
    }
    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        long_lasting_tree.check_tree()
    );

    println!("time: {}ms", start.elapsed().as_millis());
}

fn main() {
    bench_parallel();
}