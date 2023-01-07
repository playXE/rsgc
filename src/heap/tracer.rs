use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum GCCause {
    Explicit,
    AllocationFailure,
    ConcurrentGC,
    UpgradeToFullGC,
    LastGCCause,
}

impl GCCause {
    pub fn is_allocation_failure(&self) -> bool {
        match self {
            GCCause::AllocationFailure => true,
            _ => false,
        }
    }

    pub fn is_user_requested(&self) -> bool {
        match self {
            GCCause::Explicit => true,
            _ => false,
        }
    }
}

pub struct SharedGCInfo {
    name: &'static str,
    cause: GCCause,
    start_timestamp: Instant,
    end_timestap: Instant,
    sum_of_pauses: Duration,
    longest_pause: Duration,
}

impl SharedGCInfo {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            cause: GCCause::LastGCCause,
            start_timestamp: Instant::now(),
            end_timestap: Instant::now(),
            sum_of_pauses: Duration::new(0, 0),
            longest_pause: Duration::new(0, 0),
        }
    }

    pub fn set_start_timestamp(&mut self, start_timestamp: Instant) {
        self.start_timestamp = start_timestamp;
    }

    pub fn set_end_timestamp(&mut self, end_timestamp: Instant) {
        self.end_timestap = end_timestamp;
    }

    pub fn set_sum_of_pauses(&mut self, sum_of_pauses: Duration) {
        self.sum_of_pauses = sum_of_pauses;
    }

    pub fn set_longest_pause(&mut self, longest_pause: Duration) {
        self.longest_pause = longest_pause;
    }

    pub fn set_cause(&mut self, cause: GCCause) {
        self.cause = cause;
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn cause(&self) -> GCCause {
        self.cause
    }

    pub fn start_timestamp(&self) -> Instant {
        self.start_timestamp
    }

    pub fn end_timestamp(&self) -> Instant {
        self.end_timestap
    }

    pub fn sum_of_pauses(&self) -> Duration {
        self.sum_of_pauses
    }

    pub fn longest_pause(&self) -> Duration {
        self.longest_pause
    }

    pub fn duration(&self) -> Duration {
        self.end_timestap - self.start_timestamp
    }

    pub fn is_user_requested(&self) -> bool {
        self.cause.is_user_requested()
    }

    pub fn is_allocation_failure(&self) -> bool {
        self.cause.is_allocation_failure()
    }

    pub fn is_concurrent(&self) -> bool {
        match self.cause {
            GCCause::ConcurrentGC => true,
            _ => false,
        }
    }

    pub fn is_upgrade_to_full(&self) -> bool {
        match self.cause {
            GCCause::UpgradeToFullGC => true,
            _ => false,
        }
    }
}
