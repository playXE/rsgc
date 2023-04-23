use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod rcute {
    use std::alloc::Layout;
    use std::fmt::{Debug, Formatter};
    use std::ops::Deref;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::{alloc, ptr};

    #[repr(C)]
    struct Header<T: ?Sized> {
        counter: AtomicU32,
        value: T,
    }

    #[derive(Hash)]
    pub struct Rcute<T: ?Sized> {
        value: NonNull<Header<T>>,
    }

    impl<T: Default> Default for Rcute<T> {
        fn default() -> Self {
            Rcute::new(T::default())
        }
    }

    impl<T: Debug> Debug for Rcute<T> {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.inner())
        }
    }

    impl<T> Rcute<T> {
        pub fn new(val: T) -> Self {
            let value: *mut Header<T> = unsafe { alloc::alloc(Layout::new::<Header<T>>()) }.cast();
            assert!(!value.is_null(), "Failed to allocate memory");
            unsafe {
                value.write(Header {
                    counter: AtomicU32::new(1),
                    value: val,
                });
            }
            unsafe {
                Self {
                    value: NonNull::new_unchecked(value),
                }
            }
        }

        pub fn clone(this: &Rcute<T>) -> Rcute<T> {
            unsafe {
                this.value.as_ref().counter.fetch_add(1, Ordering::Relaxed);
            }
            Rcute { value: this.value }
        }

        fn inner(&self) -> &T {
            unsafe { &self.value.as_ref().value }
        }
    }

    impl<T: ?Sized> Drop for Rcute<T> {
        fn drop(&mut self) {
            unsafe {
                self.value.as_ref().counter.fetch_sub(1, Ordering::Relaxed);
            }

            if unsafe { self.value.as_ref().counter.load(Ordering::Relaxed) == 0 } {
                unsafe {
                    ptr::drop_in_place(self.value.as_ptr());
                }
                unsafe {
                    alloc::dealloc(
                        self.value.as_ptr().cast(),
                        Layout::for_value(self.value.as_ref()),
                    );
                }
            }
        }
    }

    impl<T> Deref for Rcute<T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            self.inner()
        }
    }
}

unsafe impl<T: ?Sized + Sync + Send> Send for Rc<T> {}

unsafe impl<T: ?Sized + Sync + Send> Sync for Rc<T> {}

use rcute::Rcute as Rc;

#[allow(dead_code)]
pub struct Node {
    item: i64,
    left: Option<Rc<Node>>,
    right: Option<Rc<Node>>,
}

impl Node {
    fn check_tree(&self) -> usize {
        if self.left.is_none() {
            return 1;
        }

        1 + self.left.as_ref().unwrap().check_tree() + self.right.as_ref().unwrap().check_tree()
    }
}

fn create_tree(depth: i64) -> Rc<Node> {
    if 0 < depth {
        let node = Rc::new(Node {
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

        Rc::new(node)
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

    let mut d = min_depth;

    while d <= max_depth {
        let depth = d;

        let iterations = 1 << (max_depth - depth + min_depth);
        let mut check = 0;
        for _ in 1..=iterations {
            let tree_node = create_tree(depth as _);
            check += tree_node.check_tree();
        }

        println!(
            "{}\t trees of depth {}\t check: {}",
            iterations, depth, check
        );

        d += 2;
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
