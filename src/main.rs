use rsgc::heap::heap::Heap;

fn main() {
   env_logger::init();

   let mut heap = Heap::new(96 * 1024 * 1024,
      None,
      None,
      None,
      None,
      None,
      None,
      false,
      10, 10);

      heap.should_start_gc();
      heap.free_set().log_status();
}