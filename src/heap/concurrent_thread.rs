pub trait ConcurrentGCThread {
    fn should_terminate(&self) -> bool;
    fn has_terminated(&self) -> bool;

    fn create_and_start(&mut self);

    fn run_service(&mut self);
    fn stop_service(&mut self);

    fn run(&mut self);
    fn stop(&mut self);
}

