use rsgc::{prelude::*, formatted_size};
use system::array::Array;

fn create_message(thread: &mut Thread, n: usize) -> Handle<Array<u8>> {
    let msg = Array::new(thread,MSG_SIZE, |_, _| n as u8);

    msg
}

const WINDOW_SIZE: usize = 200_000;
const MSG_COUNT: usize = 1_000_000;
const MSG_SIZE: usize = 1024;

#[inline(never)]
fn push_message(
    thread: &mut Thread,
    store: &mut Handle<Array<Handle<Array<u8>>>>,
    id: usize,
    worst: &mut std::time::Duration,
) {
    let start = std::time::Instant::now();
    let msg = create_message(thread, id);
    thread.write_barrier(*store);
    store[id % WINDOW_SIZE] = msg;
    let elapsed = start.elapsed();
    if elapsed > *worst {
        *worst = elapsed;
    }
}

fn main() {
    let mut worst = std::time::Duration::from_nanos(0);

    let res = std::panic::catch_unwind(move || {
        let _ = main_thread(HeapArguments::from_env(), move |heap| {
            heap.add_core_root_set();
            let thread = Thread::current();
            let mut store = Array::new(thread, WINDOW_SIZE, |thread, _| {
                Array::new(thread, MSG_SIZE, |_, _| 0)
            });


            for i in 0..MSG_COUNT {
                push_message(thread, &mut store, i, &mut worst);
                thread.safepoint();
            }

            println!("worst push time: {:.02}ms", worst.as_micros() as f64 / 1000.0);
            Ok(())
        });
    });

    match res {
        Ok(_) => (),
        Err(e) => {
            if let Some(oom) = e.downcast_ref::<OOM>() {
                println!("OOM: {}", formatted_size(oom.0));
            } else {
                println!("panic: {:?}", e);
            }
        }
    }
}
