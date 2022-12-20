use std::{
    sync::atomic::AtomicIsize,
    time::{Duration, Instant},
};

use atomic::{Atomic, Ordering};

use crate::{formatted_size, sync::monitor::Monitor, utils::number_seq::TruncatedSeq};

use super::{heap::heap, shared_vars::SharedFlag};

/// Pacer provides allocation pacing mechanism.
///
///
/// Currently it implements simple tax-and-spend pacing policy: GC threads provide
/// credit, allocating thread spend the credit, or stall when credit is not available.
pub struct Pacer {
    last_time: Instant,
    progress_history: Box<TruncatedSeq>,
    wait_monitor: Box<Monitor<()>>,
    need_notify_waiters: SharedFlag,
    epoch: AtomicIsize,
    tax_rate: Atomic<f64>,
    budget: AtomicIsize,
    progress: AtomicIsize,
}

impl Pacer {
    pub fn new() -> Self {
        Self {
            last_time: Instant::now(),
            progress_history: Box::new(TruncatedSeq::new(5, 0.3)),
            wait_monitor: Box::new(Monitor::new(())),
            need_notify_waiters: SharedFlag::new(),
            epoch: AtomicIsize::new(0),
            tax_rate: Atomic::new(0.0),
            budget: AtomicIsize::new(0),
            progress: AtomicIsize::new(-1),
        }
    }

    fn report_progress_internal(&self, bytes: usize) {
        self.progress.fetch_add(bytes as isize, Ordering::Relaxed);
    }

    fn add_budget(&self, bytes: usize) {
        let inc = bytes as isize;
        let new_budget = self.budget.fetch_add(inc, Ordering::Relaxed) + inc;

        // Was the budget replenished beyond zero? Then all pacing claims
        // are satisfied, notify the waiters. Avoid taking any locks here,
        // as it can be called from hot paths and/or while holding other locks.
        if new_budget >= 0 && (new_budget - inc) < 0 {
            self.need_notify_waiters.try_set();
        }
    }

    fn report_internal(&self, bytes: usize) {
        self.add_budget(bytes);
    }

    pub fn report_alloc(&self, bytes: usize) {
        self.report_internal(bytes);
    }

    pub fn report_mark(&self, bytes: usize) {
        self.report_internal(bytes);
        self.report_progress_internal(bytes);
    }

    pub fn report_sweep(&self, bytes: usize) {
        self.report_internal(bytes);
    }

    pub fn update_and_get_progress_history(&mut self) -> usize {
        if self.progress.load(Ordering::Relaxed) < 0 {
            self.progress.store(0, Ordering::Relaxed);
            return (heap().max_capacity() as f64 * 0.1) as usize;
        } else {
            self.progress_history
                .add(self.progress.load(Ordering::Relaxed) as _);
            self.progress.store(0, Ordering::Relaxed);

            self.progress_history.avg() as usize
        }
    }

    pub fn restart_with(&self, non_taxable_bytes: usize, tax_rate: f64) {
        let initial = (non_taxable_bytes as f64 * tax_rate) as isize;
        self.budget.swap(initial, Ordering::Relaxed);
        self.tax_rate.store(tax_rate, Ordering::Relaxed);
        self.epoch.fetch_add(1, Ordering::Relaxed);

        self.need_notify_waiters.try_set();
    }

    pub fn setup_for_mark(&mut self) {
        let heap = heap();
        let live = self.update_and_get_progress_history();
        let free = heap.free_set().available();

        let non_taxable = free * heap.options().pacing_cycle_slack / 100;
        let taxable = free - non_taxable;

        let mut tax = 1.0 * live as f64 / taxable as f64;
        tax *= 1.0;
        tax *= heap.options().pacing_surcharge;

        self.restart_with(non_taxable, tax);

        log::info!(target: "gc", "Pacer for Mark. Expected Live: {}, Free: {}, Non-Taxable: {}, Alloc Tax Rate: {:.01}",
            formatted_size(live),
            formatted_size(free),
            formatted_size(non_taxable),
            tax
        );
    }

    pub fn setup_for_sweep(&mut self) {
        let heap = heap();

        let used = heap.free_set().used();
        let free = heap.free_set().available();

        let non_taxable = free * heap.options().pacing_cycle_slack / 100;
        let taxable = free - non_taxable;

        let mut tax = 1.0 * used as f64 / taxable as f64;
        tax *= 1.0;
        tax = 1.0f64.max(tax);
        tax *= heap.options().pacing_surcharge;

        self.restart_with(non_taxable, tax);
        log::info!(target: "gc", "Pacer for Sweep. Used: {}, Free: {}, Non-Taxable: {}, Alloc Rate Tax: {:.01}",
            formatted_size(used),
            formatted_size(free),
            formatted_size(non_taxable),
            tax
        );
    }

    pub fn setup_for_idle(&mut self) {
        let heap = heap();
        let initial = heap.max_capacity() / 100 * heap.options().pacing_idle_slack;

        let tax = 1.0;

        self.restart_with(initial, tax);

        log::info!(target: "gc", "Pacer for Idle. Initial: {}, Alloc Rate Tax: {:.01}",
            formatted_size(initial),
            tax
        );
    }

    pub fn claim_for_alloc(&self, bytes: usize, force: bool) -> bool {
        let tax = 1.0f64.max(bytes as f64 * self.tax_rate.load(Ordering::Relaxed)) as isize;
        let mut cur;
        let mut new_val;
        loop {
            cur = self.budget.load(Ordering::Relaxed);

            if cur < tax && !force {
                return false;
            }

            new_val = cur - tax;

            match self.budget.compare_exchange_weak(
                cur,
                new_val,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break true,
                Err(_) => continue,
            }
        }
    }

    pub fn update_for_alloc(&self, epoch: isize, bytes: usize) {
        if self.epoch.load(Ordering::Relaxed) != epoch {
            return;
        }

        // Fast path: try to allocate right away
        let claimed = self.claim_for_alloc(bytes, false);

        if claimed {
            return;
        }

        // Forcefully claim the budget: it may go negative at this point, and
        // GC should replenish for this and subsequent allocations. After this claim,
        // we would wait a bit until our claim is matched by additional progress,
        // or the time budget depletes.
        let claimed = self.claim_for_alloc(bytes, true);
        assert!(claimed, "should always succeed");
        let start = Instant::now();
        let max = Duration::from_millis(heap().options().pacing_max_delay as _);
        let mut total = Duration::from_millis(0);

        loop {
            let cur = if max > total {
                max - total
            } else {
                Duration::from_millis(1)
            };

            self.wait(cur);
            let end = Instant::now();
            total = end - start;
            if total > max || self.budget.load(Ordering::Relaxed) >= 0 {
                // Exiting if either:
                //  a) Spent local time budget to wait for enough GC progress.
                //     Breaking out and allocating anyway, which may mean we outpace GC,
                //     and start Degenerated GC cycle.
                //  b) The budget had been replenished, which means our claim is satisfied.
                break;
            }
        }
    }

    pub fn notify_waiters(&self) {
        if self.need_notify_waiters.try_unset() {
            let locker = self.wait_monitor.lock(true);
            locker.notify_all();
        }
    }

    fn wait(&self, time: Duration) {
        let mut locker = self.wait_monitor.lock(true);
        locker.wait_for(time);
    }
}
