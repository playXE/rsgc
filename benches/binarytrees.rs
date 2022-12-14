extern crate rsgc;

use rsgc::{
    env::read_uint_from_env,
    heap::{heap::Heap, region::HeapArguments, thread::ThreadInfo},
    object::{Allocation, Handle},
    traits::Object,
};

use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};

mod rsgc_ {
    use super::*;
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
    pub fn bench(max_depth: usize) {
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
}

mod rc {
    use std::rc::Rc;

    #[allow(dead_code)]
    pub struct TreeNode {
        item: i64,
        left: Option<Rc<Self>>,
        right: Option<Rc<Self>>,
    }

    impl TreeNode {
        fn check_tree(&self) -> usize {
            match (self.left.as_ref(), self.right.as_ref()) {
                (Some(ref left), Some(ref right)) => left.check_tree() + right.check_tree() + 1,
                _ => 1,
            }
        }
    }

    fn create_tree(mut depth: i64) -> Rc<TreeNode> {
        if depth > 0 {
            depth -= 1;

            Rc::new(TreeNode {
                item: depth + 1,
                left: Some(create_tree(depth)),
                right: Some(create_tree(depth)),
            })
        } else {
            Rc::new(TreeNode {
                item: depth,
                left: None,
                right: None,
            })
        }
    }

    fn loops(iterations: i64, depth: i64) {
        let mut check = 0;
        let mut item = 0;
        let th = rsgc::heap::thread::thread();
        while {
            th.safepoint();
            check += create_tree(depth).check_tree();
            item += 1;
            item < iterations
        } {}

        println!(
            "{}\t trees of depth {}\t check: {}",
            iterations, depth, check
        );
    }

    fn trees(max_depth: i64) {
        let long_lasting_tree = create_tree(max_depth);

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
    pub fn bench(max_depth: usize) {
        let start = std::time::Instant::now();
        let stretch_depth = max_depth + 1;

        {
            println!(
                "stretch tree of depth {}\t check: {}",
                stretch_depth,
                create_tree(stretch_depth as _).as_ref().check_tree()
            );
        }

        trees(max_depth as _);

        println!(
            "binary trees took: {} secs",
            start.elapsed().as_micros() as f64 / 1000.0 / 1000.0
        );
    }
}

mod gc_ {
    use gc::Gc;
    use gc_derive::{Finalize, Trace};

    #[derive(Trace, Finalize)]
    pub struct TreeNode {
        item: i64,
        left: Option<Gc<TreeNode>>,
        right: Option<Gc<TreeNode>>,
    }

    impl TreeNode {
        fn check_tree(&self) -> usize {
            match (self.left.as_ref(), self.right.as_ref()) {
                (Some(ref left), Some(ref right)) => left.check_tree() + right.check_tree() + 1,
                _ => 1,
            }
        }
    }

    fn create_tree(mut depth: i64) -> Gc<TreeNode> {
        if depth > 0 {
            depth -= 1;

            Gc::new(TreeNode {
                item: depth + 1,
                left: Some(create_tree(depth)),
                right: Some(create_tree(depth)),
            })
        } else {
            Gc::new(TreeNode {
                item: depth,
                left: None,
                right: None,
            })
        }
    }

    fn loops(iterations: i64, depth: i64) {
        let mut check = 0;
        let mut item = 0;
        let th = rsgc::heap::thread::thread();
        while {
            th.safepoint();
            check += create_tree(depth).check_tree();
            item += 1;
            item < iterations
        } {}

        println!(
            "{}\t trees of depth {}\t check: {}",
            iterations, depth, check
        );
    }

    fn trees(max_depth: i64) {
        let long_lasting_tree = create_tree(max_depth);

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
    pub fn bench(max_depth: usize) {
        let start = std::time::Instant::now();
        let stretch_depth = max_depth + 1;

        {
            println!(
                "stretch tree of depth {}\t check: {}",
                stretch_depth,
                create_tree(stretch_depth as _).as_ref().check_tree()
            );
        }

        trees(max_depth as _);

        println!(
            "binary trees took: {} secs",
            start.elapsed().as_micros() as f64 / 1000.0 / 1000.0
        );
    }
}

pub fn criterion_benchmark(c: &mut Criterion) {
    env_logger::init();
    let init_depth = match read_uint_from_env("TREE_DEPTH") {
        Some(x) if x > 6 => x,
        _ => 6,
    };
    let max_depth = match read_uint_from_env("MAX_TREE_DEPTH") {
        Some(x) if x > 6 => x,
        _ => init_depth + 2,
    };
    let args = HeapArguments::from_env();
    let _ = Heap::new(args);
    let mut group = c.benchmark_group("binary trees");
    group.sample_size(10);

    for max_depth in init_depth..=max_depth {
        group.bench_with_input(
            BenchmarkId::new("Binary trees (RSGC)", max_depth),
            &max_depth,
            |b, input| {
                b.iter(|| {
                    rsgc_::bench(*input);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("Binary trees (std::rc::Rc)", max_depth),
            &max_depth,
            |b, input| {
                b.iter(|| {
                    rc::bench(*input);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("Binary trees (gc::Gc)", max_depth),
            &max_depth,
            |b, input| {
                b.iter(|| {
                    gc_::bench(*input);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
