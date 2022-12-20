use std::time::{Duration, Instant};

use crate::{
    formatted_size, formatted_sizef,
    heap::{heap::heap, region::HeapOptions},
    utils::number_seq::TruncatedSeq,
};

use super::{Heuristics, CONCURRENT_ADJUST, DEGENERATED_PENALTY, FULL_PENALTY};

/// Used to record the last trigger that signaled to start a GC.
/// This itself is used to decide whether or not to adjust the margin of
/// error for the average cycle time and allocation rate or the allocation
/// spike detection threshold.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Trigger {
    Spike,
    Rate,
    Other,
}

// These constants are used to adjust the margin of error for the moving
// average of the allocation rate and cycle time. The units are standard
// deviations.
pub const FULL_PENALTY_SD: f64 = 0.2;
pub const DEGENERATE_PENALTY_SD: f64 = 0.1;

// These are used to decide if we want to make any adjustments at all
// at the end of a successful concurrent cycle.
pub const LOWEST_EXPECTED_AVAILABLE_AT_END: f64 = -0.5;
pub const HIGHEST_EXPECTED_AVAILABLE_AT_END: f64 = 0.5;

// These values are the confidence interval expressed as standard deviations.
// At the minimum confidence level, there is a 25% chance that the true value of
// the estimate (average cycle time or allocation rate) is not more than
// MINIMUM_CONFIDENCE standard deviations away from our estimate. Similarly, the
// MAXIMUM_CONFIDENCE interval here means there is a one in a thousand chance
// that the true value of our estimate is outside the interval. These are used
// as bounds on the adjustments applied at the outcome of a GC cycle.
pub const MINIMUM_CONFIDENCE: f64 = 0.319;
pub const MAXIMUM_CONFIDENCE: f64 = 3.291;

struct AllocationRate {
    last_sample_time: Instant,
    last_sample_value: usize,
    interval: Duration,
    rate: TruncatedSeq,
    rate_avg: TruncatedSeq,
}

impl AllocationRate {
    pub fn new(opts: &HeapOptions) -> Self {
        let this = Self {
            last_sample_time: Instant::now(),
            last_sample_value: 0,
            interval: Duration::from_millis(
                ((1.0 / opts.adaptive_sample_frequency_hz as f64) * 1000.0) as u64,
            ),
            rate: TruncatedSeq::new(
                opts.adaptive_sample_frequency_size_seconds * opts.adaptive_sample_frequency_hz,
                opts.adaptive_decay_factor,
            ),
            rate_avg: TruncatedSeq::new(
                opts.adaptive_sample_frequency_size_seconds * opts.adaptive_sample_frequency_hz,
                opts.adaptive_decay_factor,
            ),
        };
        log::info!(target: "gc", "Sample interval: {} ms", this.interval.as_millis());
        this
    }

    pub fn sample(&mut self, allocated: usize) -> f64 {
        let now = Instant::now();

        let mut rate = 0.0;

        if now - self.last_sample_time > self.interval {
            if allocated >= self.last_sample_value {
                rate = self.instanteneous_rate_(now, allocated);
               
                self.rate.add(rate);
                self.rate_avg.add(self.rate.avg());
            }

            self.last_sample_time = now;
            self.last_sample_value = allocated;
        }

        rate
    }

    pub fn instanteneous_rate(&self, allocated: usize) -> f64 {
        self.instanteneous_rate_(Instant::now(), allocated)
    }

    pub fn instanteneous_rate_(&self, time: Instant, allocated: usize) -> f64 {
        let last_value = self.last_sample_value;
        let last_time = self.last_sample_time;
        let allocation_delta = if allocated > last_value {
            allocated - last_value
        } else {
            0
        };

        let time_delta = time - last_time;
        //println!("time delta: {}ms {}", time_delta.as_micros() as f64 / 1000.0, formatted_size(allocation_delta));
        if time_delta.as_micros() as f64 / 1000.0 / 1000.0 > 0.0 {
            allocation_delta as f64 / (time_delta.as_micros() as f64 / 1000.0 / 1000.0)
        } else {
            0.0
        }
    }

    pub fn upper_bound(&self, sds: f64) -> f64 {
        // Here we are using the standard deviation of the computed running
        // average, rather than the standard deviation of the samples that went
        // into the moving average. This is a much more stable value and is tied
        // to the actual statistic in use (moving average over samples of averages).
        self.rate.davg() + (sds * self.rate_avg.dsd())
    }

    pub fn allocation_counter_reset(&mut self) {
        self.last_sample_time = Instant::now();
        self.last_sample_value = 0;
    }

    pub fn is_spiking(&self, rate: f64, threshold: f64) -> bool {
        if rate <= 0.0 {
            return false;
        }

        let sd = self.rate.sd();

        if sd > 0.0 {
            // There is a small chance that that rate has already been sampled, but it
            // seems not to matter in practice.
            let z_score = (rate - self.rate.avg()) / sd;

            if z_score > threshold {
                return true;
            }
        }

        false
    }
}

fn saturate(value: f64, min: f64, max: f64) -> f64 {
    value.min(max).max(min)
}

pub struct AdaptiveHeuristics {
    degenerated_cycles_in_a_row: usize,
    successful_cycles_in_a_row: usize,
    cycle_start: Instant,
    last_cycle_end: Instant,

    gc_times_learned: usize,
    gc_time_penalties: isize,
    gc_time_history: Box<TruncatedSeq>,

    allocation_rate: AllocationRate,
    /// The margin of error expressed in standard deviations to add to our
    /// average cycle time and allocation rate. As this value increases we
    /// tend to over estimate the rate at which mutators will deplete the
    /// heap. In other words, erring on the side of caution will trigger more
    /// concurrent GCs.
    margin_of_error_sd: f64,

    /// The allocation spike threshold is expressed in standard deviations.
    /// If the standard deviation of the most recent sample of the allocation
    /// rate exceeds this threshold, a GC cycle is started. As this value
    /// decreases the sensitivity to allocation spikes increases. In other
    /// words, lowering the spike threshold will tend to increase the number
    /// of concurrent GCs.
    spike_threshold_sd: f64,

    available: TruncatedSeq,
    /// Remember which trigger is responsible for the last GC cycle. When the
    /// outcome of the cycle is evaluated we will adjust the parameters for the
    /// corresponding triggers. Note that successful outcomes will raise
    /// the spike threshold and lower the margin of error.
    last_trigger: Trigger,
}

impl AdaptiveHeuristics {
    pub fn new(opts: &HeapOptions) -> Box<dyn Heuristics> {
        Box::new(Self {
            degenerated_cycles_in_a_row: 0,
            successful_cycles_in_a_row: 0,
            cycle_start: Instant::now(),
            last_cycle_end: Instant::now(),

            gc_times_learned: 0,
            gc_time_penalties: 0,
            gc_time_history: Box::new(TruncatedSeq::new(10, opts.adaptive_decay_factor)),
            margin_of_error_sd: opts.adaptive_initial_confidence,
            spike_threshold_sd: opts.adaptive_initial_spike_threshold,

            available: TruncatedSeq::new(10, 0.3),
            allocation_rate: AllocationRate::new(opts),
            last_trigger: Trigger::Other,
        })
    }

    fn adjust_margin_of_error(&mut self, amount: f64) {
        self.margin_of_error_sd = saturate(
            self.margin_of_error_sd + amount,
            MINIMUM_CONFIDENCE,
            MAXIMUM_CONFIDENCE,
        );
    }

    fn adjust_spike_threshold(&mut self, amount: f64) {
        self.spike_threshold_sd = saturate(
            self.spike_threshold_sd + amount,
            MINIMUM_CONFIDENCE,
            MAXIMUM_CONFIDENCE,
        );
    }

    fn adjust_last_trigger_parameters(&mut self, amount: f64) {
        match self.last_trigger {
            Trigger::Rate => self.adjust_margin_of_error(amount),
            Trigger::Spike => self.adjust_spike_threshold(amount),
            _ => (),
        }
    }
}

impl Heuristics for AdaptiveHeuristics {
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
        let allocated = heap.bytes_allocated_since_gc_start();

        let rate = self.allocation_rate.sample(allocated);
        self.last_trigger = Trigger::Other;

        let min_threshold = max_capacity / 100 * heap.options().min_free_threshold;

        if available < min_threshold {
            log::info!(target: "gc", "Trigger: Free ({}) is below minimum threshold ({})",
                formatted_size(available),
                formatted_size(min_threshold)
            );

            return true;
        }

        let max_learn = heap.options().learning_steps;

        if self.gc_times_learned < max_learn {
            let init_threshold = max_capacity / 100 * heap.options().init_free_threshold;

            if available < init_threshold {
                log::info!(target: "gc", "Trigger: Learning {} of {}. Free ({}) is below initial threshold ({})",
                    self.gc_times_learned + 1,
                    max_learn,
                    formatted_size(available),
                    formatted_size(init_threshold)
                );

                return true;
            }
        }
        // Check if allocation headroom is still okay. This also factors in:
        //   1. Some space to absorb allocation spikes
        //   2. Accumulated penalties from Degenerated and Full GC
        let mut allocation_headroom = available;
        let avg_alloc_rate = self.allocation_rate.upper_bound(self.margin_of_error_sd);
        let spike_headroom = max_capacity / 100 * heap.options().alloc_spike_factor;
        let penalties = max_capacity / 100 * self.gc_time_penalties as usize;

        allocation_headroom -= allocation_headroom.min(spike_headroom);
        allocation_headroom -= allocation_headroom.min(penalties);

        let avg_cycle_time =
            self.gc_time_history.davg() + (self.margin_of_error_sd * self.gc_time_history.dsd());
        //log::info!("avg_cycle_time: {}, allocation rate: {}, {}", avg_cycle_time, formatted_sizef(avg_alloc_rate), allocation_headroom as f64 / avg_alloc_rate);

        if avg_cycle_time > allocation_headroom as f64 / avg_alloc_rate {
            log::info!(
                target: "gc",
                "Trigger: Average GC time ({:.02} ms) is above the time for average allocation rate ({}B/s) to deplete free headroom ({}) (margin of error = {:.02}%)",
                avg_cycle_time * 1000.0,
                formatted_sizef(avg_alloc_rate),
                formatted_size(allocation_headroom),
                self.margin_of_error_sd
            );

            log::info!(
                target: "gc",
                "Free headroom: {} (free) - {} (spike) - {} (penalties) = {}",
                formatted_size(available),
                formatted_size(spike_headroom),
                formatted_size(penalties),
                formatted_size(allocation_headroom)
            );

            self.last_trigger = Trigger::Rate;
            return true;
        }
        //println!("rate: {}", rate);
        let is_spiking = self
            .allocation_rate
            .is_spiking(rate, self.spike_threshold_sd);

        if is_spiking && avg_cycle_time > allocation_headroom as f64 / rate {
            log::info!(
                target: "gc",
                "Trigger: Average GC time ({:.02} ms) is above the time for instanteneous allocation rate ({}B/s) to deplete free headroom ({}) (spike threshold = {:.02})",
                avg_cycle_time * 1000.0,
                formatted_sizef(rate),
                formatted_size(allocation_headroom),
                self.spike_threshold_sd
            );

            self.last_trigger = Trigger::Spike;
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

    fn record_success_concurrent(&mut self) {
        self.set_degenerate_cycles_in_a_row(0);
        self.set_successful_cycles_in_a_row(self.successful_cycles_in_a_row() + 1);

        let t = self.time_since_last_gc().as_micros() as f64 / 1000.0 / 1000.0;
        self.gc_time_history_mut().add(t);
        self.set_gc_times_learned(self.gc_times_learned() + 1);

        self.adjust_penalty(CONCURRENT_ADJUST);

        let available = heap().free_set().available();

        self.available.add(available as _);

        let mut z_score = 0.0;

        if self.available.sd() > 0.0 {
            z_score = (available as f64 - self.available.avg()) / self.available.sd();
        }

        log::debug!(target: "gc",
            "Available: {}, z-score={:.03}. Average available: {:.01} +/- {:.01}.",
            formatted_size(available),
            z_score,
            formatted_sizef(self.available.avg()),
            formatted_sizef(self.available.sd())
        );

        // In the case when a concurrent GC cycle completes successfully but with an
        // unusually small amount of available memory we will adjust our trigger
        // parameters so that they are more likely to initiate a new cycle.
        // Conversely, when a GC cycle results in an above average amount of available
        // memory, we will adjust the trigger parameters to be less likely to initiate
        // a GC cycle.
        //
        // The z-score we've computed is in no way statistically related to the
        // trigger parameters, but it has the nice property that worse z-scores for
        // available memory indicate making larger adjustments to the trigger
        // parameters. It also results in fewer adjustments as the application
        // stabilizes.
        //
        // In order to avoid making endless and likely unnecessary adjustments to the
        // trigger parameters, the change in available memory (with respect to the
        // average) at the end of a cycle must be beyond these threshold values.

        if z_score < LOWEST_EXPECTED_AVAILABLE_AT_END || z_score > HIGHEST_EXPECTED_AVAILABLE_AT_END
        {
            // The sign is flipped because a negative z-score indicates that the
            // available memory at the end of the cycle is below average. Positive
            // adjustments make the triggers more sensitive (i.e., more likely to fire).
            // The z-score also gives us a measure of just how far below normal. This
            // property allows us to adjust the trigger parameters proportionally.
            //
            // The `100` here is used to attenuate the size of our adjustments. This
            // number was chosen empirically. It also means the adjustments at the end of
            // a concurrent cycle are an order of magnitude smaller than the adjustments
            // made for a degenerated or full GC cycle (which themselves were also
            // chosen empirically).
            self.adjust_last_trigger_parameters(z_score / -100.0);
        }
    }

    fn record_success_degenerated(&mut self) {
        self.set_degenerate_cycles_in_a_row(self.degenerate_cycles_in_a_row() + 1);
        self.set_successful_cycles_in_a_row(0);
        self.adjust_penalty(DEGENERATED_PENALTY);

        self.adjust_margin_of_error(DEGENERATE_PENALTY_SD);
        self.adjust_spike_threshold(DEGENERATE_PENALTY_SD);
    }

    fn record_success_full(&mut self) {
        self.set_degenerate_cycles_in_a_row(0);
        self.set_successful_cycles_in_a_row(self.successful_cycles_in_a_row() + 1);
        self.adjust_penalty(FULL_PENALTY);

        self.adjust_margin_of_error(FULL_PENALTY_SD);
        self.adjust_spike_threshold(FULL_PENALTY_SD);
    }

    fn record_cycle_start(&mut self) {
        self.set_cycle_start(Instant::now());
        self.allocation_rate.allocation_counter_reset();
    }
}
