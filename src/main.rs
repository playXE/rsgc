use rsgc::prelude::*;

struct Node {}

impl Object for Node {}
impl Allocation for Node {
    const FINALIZE: bool = true;
}

impl Drop for Node {
    fn drop(&mut self) {
        println!("Dropping Node {:p}", self);
    }
}

fn main(){ 
    main_thread(HeapArguments::from_env(), |heap| {
        heap.add_core_root_set();
        let thread = Thread::current();

        for _ in 0..100 {
            let _ = thread.allocate(Node {});
        }
        let x = thread.allocate(Node {});
        heap.request_gc();

        println!("{:p} {:p}", x, &x);

        Ok(())
    }).unwrap();
}