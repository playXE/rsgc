use std::time::{Instant, Duration};

pub trait PhaseVisitor {
    fn visit(&mut self, phase: &GCPhase);
    fn visit_mut(&mut self, phase: &mut GCPhase);
}


#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PhaseType {
    Pause,
    Concurrent,
}

pub struct GCPhase {
    name: &'static str,
    level: i32,
    start: Instant,
    end: Instant,
    typ: PhaseType
}

impl GCPhase {
    pub fn set_name(&mut self, name: &'static str) {
        self.name = name;
    }

    pub fn set_level(&mut self, level: i32) {
        self.level = level;
    }

    pub fn set_start(&mut self, start: Instant) {
        self.start = start;
    }

    pub fn set_end(&mut self, end: Instant) {
        self.end = end;
    }

    pub fn set_type(&mut self, typ: PhaseType) {
        self.typ = typ;
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn level(&self) -> i32 {
        self.level
    }

    pub fn start(&self) -> Instant {
        self.start
    }

    pub fn end(&self) -> Instant {
        self.end
    }

    pub fn typ(&self) -> PhaseType {
        self.typ
    }

    pub fn duration(&self) -> std::time::Duration {
        self.end - self.start
    }

    pub fn new(name: &'static str, level: i32, start: Instant, end: Instant, typ: PhaseType) -> Self {
        Self {
            name,
            level,
            start,
            end,
            typ
        }
    }
    
    pub fn accept(&self, visitor: &mut impl PhaseVisitor) {
        visitor.visit(self);
    }

    pub fn accept_mut(&mut self, visitor: &mut impl PhaseVisitor) {
        visitor.visit_mut(self);
    }
}

pub struct PhasesStack {
    phase_indices: [i32; 6],
    next_phase_level: i32
}

impl PhasesStack {
    pub const fn new() -> Self {
        Self {
            phase_indices: [0; 6],
            next_phase_level: 0
        }
    }

    pub fn clear(&mut self) {
        self.next_phase_level = 0;
    }

    pub fn push(&mut self, phase_index: i32) {
        self.phase_indices[self.next_phase_level as usize] = phase_index;
        self.next_phase_level += 1;
    }

    pub fn pop(&mut self) -> i32 {
        self.next_phase_level -= 1;
        self.phase_indices[self.next_phase_level as usize]
    }

    pub fn count(&self) -> i32 {
        self.next_phase_level
    }

    pub fn phase_index(&self, level: i32) -> i32 {
        self.phase_indices[level as usize]
    }
}


pub struct TimePartitions {
    phases: Vec<Box<GCPhase>>,
    active_phases: PhasesStack,
    sum_of_pauses: Duration,
    longest_pause: Duration,
}

impl TimePartitions {
    pub fn new() -> Self {
        Self {
            phases: Vec::with_capacity(100),
            active_phases: PhasesStack::new(),
            sum_of_pauses: Duration::from_secs(0),
            longest_pause: Duration::from_secs(0),
        }
    }

    pub fn clear(&mut self) {
        self.phases.clear();
        self.active_phases.clear();
        self.sum_of_pauses = Duration::from_secs(0);
        self.longest_pause = Duration::from_secs(0);
    }

    pub fn report_gc_phase_start(&mut self, name: &'static str, time: Instant, typ: PhaseType) {
        let level = self.active_phases.count();

        let phase = GCPhase::new(
            name,
            level,
            time,
            Instant::now(),
            typ 
        );
        let index = self.phases.len();
        self.phases.push(Box::new(phase));
        self.active_phases.push(index as _);
    }

    pub fn report_gc_phase_start_top_level(&mut self, name: &'static str, time: Instant, typ: PhaseType) {
        let level = self.active_phases.count();
        assert!(level == 0, "must be a top-level phase");
        self.report_gc_phase_start(name, time, typ)
    }

    pub fn report_gc_phase_start_sub_phase(&mut self, name: &'static str, time: Instant) {
        let level = self.active_phases.count();
        assert!(level > 0, "must be a sub-phase");
        let typ = self.current_phase_type();
        self.report_gc_phase_start(name, time, typ);
    }

    pub fn report_gc_phase_end(&mut self, end: Instant) {
        let phase_index = self.active_phases.pop();
        let phase = &mut self.phases[phase_index as usize];
        phase.set_end(end);

        if phase.typ() == PhaseType::Pause && phase.level() == 0 {
            let pause = phase.end() - phase.start();

            self.sum_of_pauses += pause;
            self.longest_pause = std::cmp::max(self.longest_pause, pause);
        }
    }

    pub fn num_phases(&self) -> usize {
        self.phases.len()
    }

    pub fn phase(&self, index: usize) -> &GCPhase {
        &self.phases[index]
    }

    pub fn phase_mut(&mut self, index: usize) -> &mut GCPhase {
        &mut self.phases[index]
    }

    pub fn current_phase_type(&self) -> PhaseType {
        let index = self.active_phases.phase_index(self.active_phases.count() - 1);
        self.phases[index as usize].typ()
    }
}

pub struct GCTimer {
    gc_start: Instant,
    gc_end: Instant,
    time_partitions: TimePartitions,
    concurrent: bool
}

impl GCTimer {
    pub fn time_partitions(&self) -> &TimePartitions {
        &self.time_partitions
    }

    pub fn register_gc_start(&mut self, time: Instant) {
        self.time_partitions.clear();
        self.gc_start = time;
        if !self.concurrent {
            self.register_gc_pause_start("GC Pause", time);
        }
    }

    pub fn register_gc_end(&mut self, time: Instant) {
        if !self.concurrent {
            self.register_gc_pause_end(time);
        }
        self.gc_end = time;
    }

    pub fn register_gc_pause_start(&mut self, name: &'static str, time: Instant) {
        self.time_partitions.report_gc_phase_start_top_level(name, time, PhaseType::Pause);
    }

    pub fn register_gc_pause_end(&mut self, time: Instant) {
        self.time_partitions.report_gc_phase_end(time);
    }

    pub fn register_gc_phase_start(&mut self, name: &'static str, time: Instant) {
        self.time_partitions.report_gc_phase_start_sub_phase(name, time);
    }

    pub fn register_gc_phase_end(&mut self, time: Instant) {
        self.time_partitions.report_gc_phase_end(time);
    }

    pub fn register_gc_concurrent_start(&mut self,name: &'static str, time: Instant) {
        self.time_partitions.report_gc_phase_start_top_level(name, time, PhaseType::Concurrent);
    }

    pub fn register_gc_concurrent_end(&mut self, time: Instant) {
        self.time_partitions.report_gc_phase_end(time);
    }

    pub fn new(concurrent: bool) -> Self {
        Self {
            gc_start: Instant::now(),
            gc_end: Instant::now(),
            time_partitions: TimePartitions::new(),
            concurrent
        }
    }
}