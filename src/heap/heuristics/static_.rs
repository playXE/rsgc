use std::time::Instant;

use crate::{formatted_size, heap::{heap::heap, region::HeapOptions}, utils::number_seq::TruncatedSeq};

use super::Heuristics;

pub struct StaticHeuristics {
    degenerated_cycles_in_a_row: usize,
    successful_cycles_in_a_row: usize,
    cycle_start: Instant,
    last_cycle_end: Instant,

    gc_times_learned: usize,
    gc_time_penalties: isize,
    gc_time_history: Box<TruncatedSeq>,
}

impl StaticHeuristics {
    pub fn new(opts: &mut HeapOptions) -> Box<dyn Heuristics> {
        opts.uncommit = true;
        if opts.allocation_threshold == 0 {
            opts.allocation_threshold = 10;
        }

        if opts.immediate_threshold == 90 {
            opts.immediate_threshold = 100;
        }

        if opts.uncommit_delay == 5 * 60 * 1000 {
            opts.uncommit_delay = 1000;
        }

        if opts.guaranteed_gc_interval == 5 * 60 * 1000 {
            opts.guaranteed_gc_interval = 30000;
        }

        Box::new(Self {
            degenerated_cycles_in_a_row: 0,
            successful_cycles_in_a_row: 0,
            cycle_start: Instant::now(),
            last_cycle_end: Instant::now(),

            gc_times_learned: 0,
            gc_time_penalties: 0,
            gc_time_history: Box::new(TruncatedSeq::new(10, opts.adaptive_decay_factor)),
        })
    }
}

impl Heuristics for StaticHeuristics {
    fn gc_time_penalties(&self) -> isize {
        self.gc_time_penalties
    }

    fn set_gc_time_penalties(&mut self, val: isize) {
        self.gc_time_penalties = val;
    }

    fn gc_time_history(&self) -> &TruncatedSeq {
        self.gc_time_history.as_ref()
    }

    fn gc_time_history_mut(&mut self) -> &mut TruncatedSeq {
        self.gc_time_history.as_mut()
    }

    fn degenerate_cycles_in_a_row(&self) -> usize {
        self.degenerated_cycles_in_a_row
    }

    fn set_degenerate_cycles_in_a_row(&mut self, degenerate_cycles_in_a_row: usize) {
        self.degenerated_cycles_in_a_row = degenerate_cycles_in_a_row;
    }

    fn successful_cycles_in_a_row(&self) -> usize {
        self.successful_cycles_in_a_row
    }

    fn set_successful_cycles_in_a_row(&mut self, successful_cycles_in_a_row: usize) {
        self.successful_cycles_in_a_row = successful_cycles_in_a_row;
    }

    fn cycle_start(&self) -> Instant {
        self.cycle_start
    }

    fn set_cycle_start(&mut self, cycle_start: Instant) {
        self.cycle_start = cycle_start;
    }

    fn last_cycle_end(&self) -> Instant {
        self.last_cycle_end
    }

    fn set_last_cycle_end(&mut self, last_cycle_end: Instant) {
        self.last_cycle_end = last_cycle_end;
    }

    fn gc_times_learned(&self) -> usize {
        self.gc_times_learned
    }

    fn set_gc_times_learned(&mut self, gc_times_learned: usize) {
        self.gc_times_learned = gc_times_learned;
    }

    fn should_start_gc(&mut self) -> bool {
        let heap = heap();

        let max_capacity = heap.max_capacity();
        let available = heap.free_set().available();

        let threshold_bytes_allocated = max_capacity / 100 * heap.options().allocation_threshold;
        let min_threshold = max_capacity / 100 * heap.options().min_free_threshold;

        if available < min_threshold {
            log::info!(
                "Trigger: Free ({}) is below minimum threshold ({})",
                formatted_size(available),
                formatted_size(min_threshold)
            );
            return true;
        }

        let bytes_allocated = heap.bytes_allocated_since_gc_start();

        if bytes_allocated > threshold_bytes_allocated {
            log::info!(
                "Trigger: Allocated since last cycle ({}) is larger than allocation threshold ({})",
                formatted_size(bytes_allocated),
                formatted_size(threshold_bytes_allocated)
            );
            return true;
        }

        if heap.options().guaranteed_gc_interval > 0 {
            let last_time_ms = (Instant::now() - self.last_cycle_end()).as_millis();

            if last_time_ms > heap.options().guaranteed_gc_interval as u128 {
                log::info!(target: "gc", "Trigger: Time since last GC ({} ms) is larger than guaranteed interval ({} ms)", last_time_ms, heap.options().guaranteed_gc_interval);
                return true;
            }
        }

        false
    }
}
