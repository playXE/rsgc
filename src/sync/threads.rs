use std::{thread::{Builder, JoinHandle}, sync::Arc};

use super::monitor::Monitor;

pub fn spawn_thread_and_wait_for_running<F, R>(builder: Builder, f: F) -> JoinHandle<R>
where R: Send + 'static,
      F: FnOnce() -> R + Send + 'static,
{
    let spawn_monitor = Arc::new(Monitor::new(false));
    let spawn_monitor2 = spawn_monitor.clone();
    let handle = builder.spawn(move || {
        {
            let mut ml = spawn_monitor2.lock(false);
            **ml = true;
            assert!(ml.notify(), "there must be waiting thread");
        }
        f()
    }).unwrap();
    
    let mut ml = spawn_monitor.lock(false);
    while !**ml {
        ml.wait();
    }

    handle
}