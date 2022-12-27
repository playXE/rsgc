use std::time::{Duration, Instant};

use crate::utils::number_seq::TruncatedSeq;

use super::heap::heap;

pub const CONCURRENT_ADJUST: isize = -1; // recover from penalties
pub const DEGENERATED_PENALTY: isize = 10; // how much to penalize average GC duration history on Degenerated GC
pub const FULL_PENALTY: isize = 20; // how much to penalize average GC duration history on Full GC

pub trait Heuristics {
    fn gc_time_penalties(&self) -> isize;
    fn set_gc_time_penalties(&mut self, val: isize);
    fn gc_time_history(&self) -> &TruncatedSeq;
    fn gc_time_history_mut(&mut self) -> &mut TruncatedSeq;

    fn degenerate_cycles_in_a_row(&self) -> usize;
    fn set_degenerate_cycles_in_a_row(&mut self, degenerate_cycles_in_a_row: usize);

    fn successful_cycles_in_a_row(&self) -> usize;
    fn set_successful_cycles_in_a_row(&mut self, successful_cycles_in_a_row: usize);

    fn cycle_start(&self) -> Instant;
    fn set_cycle_start(&mut self, cycle_start: Instant);

    fn last_cycle_end(&self) -> Instant;
    fn set_last_cycle_end(&mut self, last_cycle_end: Instant);

    fn gc_times_learned(&self) -> usize;
    fn set_gc_times_learned(&mut self, gc_times_learned: usize);

    fn record_cycle_start(&mut self) {
        self.set_cycle_start(Instant::now());
    }

    fn record_cycle_end(&mut self) {
        self.set_last_cycle_end(Instant::now());
    }

    fn should_start_gc(&mut self) -> bool {
        let heap = heap();

        if heap.options().guaranteed_gc_interval > 0 {
            let last_time_ms = (Instant::now() - self.last_cycle_end()).as_millis();

            if last_time_ms > heap.options().guaranteed_gc_interval as u128 {
                log::info!(target: "gc", "Trigger: Time since last GC ({} ms) is larger than guaranteed interval ({} ms)", last_time_ms, heap.options().guaranteed_gc_interval);
                return true;
            }
        }

        false
    }
    fn should_degenerate_cycle(&self) -> bool {
        self.degenerate_cycles_in_a_row() <= heap().options().full_gc_threshold
    }

    fn record_success_concurrent(&mut self) {
        self.set_degenerate_cycles_in_a_row(0);
        self.set_successful_cycles_in_a_row(self.successful_cycles_in_a_row() + 1);

        let t = self.time_since_last_gc().as_micros() as f64 / 1000.0;
        self.gc_time_history_mut().add(t);
        self.set_gc_times_learned(self.gc_times_learned() + 1);

        self.adjust_penalty(CONCURRENT_ADJUST);
    }
    fn record_success_degenerated(&mut self) {
        self.set_degenerate_cycles_in_a_row(self.degenerate_cycles_in_a_row() + 1);
        self.set_successful_cycles_in_a_row(0);
        self.adjust_penalty(DEGENERATED_PENALTY);
    }

    fn record_success_full(&mut self) {
        self.set_degenerate_cycles_in_a_row(0);
        self.set_successful_cycles_in_a_row(self.successful_cycles_in_a_row() + 1);
        self.adjust_penalty(FULL_PENALTY);
    }
    fn record_allocation_failure_gc(&mut self) {
        // no-op
    }
    fn record_requested_gc(&mut self) {
        // Assume users call System.gc() when external state changes significantly,
        // which forces us to re-learn the GC timings and allocation rates.
        self.set_gc_times_learned(0);
    }

    fn time_since_last_gc(&self) -> Duration {
        self.cycle_start().elapsed()
    }

    fn adjust_penalty(&mut self, step: isize) {
        let mut new_val = self.gc_time_penalties() + step;

        if new_val < 0 {
            new_val = 0;
        }

        if new_val > 100 {
            new_val = 100;
        }

        self.set_gc_time_penalties(new_val);
    }
}


pub mod static_;
pub mod adaptive;