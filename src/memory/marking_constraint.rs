use bitvec::vec::BitVec;

use super::visitor::{Visitor, VisitCounter};

pub trait MarkingConstraintTrait {
    fn execute(&mut self, visitor: &mut dyn Visitor);
    

    fn prepare_to_execute(&mut self, visitor: &mut dyn Visitor) {
        let _ = visitor;
    }

    fn quick_work_estimate(&mut self, visitor: &mut dyn Visitor) -> f64 {
        0.0
    }
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub enum ConstraintVolatility {
    /// The constraint needs to be validated, but it is unlikely to ever produce information.
    /// It's best to run it at the bitter end.
    SeldomGreyed,

    /// The constraint needs to be reevaluated anytime the mutator runs: so at GC start and
    /// whenever the GC resuspends after a resumption. This is almost always something that
    /// you'd call a "root" in a traditional GC.
    GreyedByExecution,

    /// The constraint needs to be reevaluated any time any object is marked and anytime the
    /// mutator resumes.
    GreyedByMarking,
}

const VERBOSE_MARKING_CONSTRAINT: bool = false;

pub struct MarkingConstraint {
    abbreviated_name: String,
    name: String,
    last_visit_count: usize,
    pub(crate) index: usize,
    volatility: ConstraintVolatility,
    impl_: Box<dyn MarkingConstraintTrait>,
}

impl MarkingConstraint {

    pub fn work_estimate(&mut self, visitor: &mut dyn Visitor) -> f64 {
        return self.last_visit_count() as f64 + self.impl_.quick_work_estimate(visitor);
    }

    pub fn quick_work_estimate(&mut self, visitor: &mut dyn Visitor) -> f64 {
        return self.impl_.quick_work_estimate(visitor);
    }
    pub fn reset_stats(&mut self) {
        self.last_visit_count = 0;
    }

    pub fn execute(&mut self, visitor: &mut dyn Visitor) {
        let mut counter = VisitCounter::new(visitor);
        self.impl_.execute(&mut counter);

        self.last_visit_count += counter.count();

        if VERBOSE_MARKING_CONSTRAINT && counter.count() != 0 {
            eprintln!("({}) visited {} in execute", self.abbreviated_name(), counter.count());
        }
    }

    pub fn prepare_to_execute(&mut self, visitor: &mut dyn Visitor) {
        let mut counter = VisitCounter::new(visitor);
        self.impl_.execute(&mut counter);

        self.last_visit_count += counter.count();

        if VERBOSE_MARKING_CONSTRAINT && counter.count() != 0 {
            eprintln!("({}) visited {} in prepare_to_execute", self.abbreviated_name(), counter.count());
        }
    }

    pub fn execute_synchronously(&mut self, visitor: &mut dyn Visitor) {
        self.impl_.prepare_to_execute(visitor);
        self.impl_.execute(visitor);
    }

    pub fn new(
        abbreviated_name: &str,
        name: &str,
        volatility: ConstraintVolatility,
        constraint: impl MarkingConstraintTrait + 'static
    ) -> Self {
        Self {
            abbreviated_name: abbreviated_name.to_owned(),
            name: name.to_owned(),
            last_visit_count: 0,
            index: 0,
            volatility,
            impl_: Box::new(constraint),
        }
    }
    pub fn index(&self) -> usize {
        self.index
    }

    pub fn abbreviated_name(&self) -> &str {
        &self.abbreviated_name
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn last_visit_count(&self) -> usize {
        self.last_visit_count
    }

    pub fn volatility(&self) -> ConstraintVolatility {
        self.volatility
    }
}

pub struct SimpleMarkingConstraint {
    executor: Box<dyn FnMut(&mut dyn Visitor)>,
}

impl SimpleMarkingConstraint {
    pub fn new<F>(executor: F) -> Self
    where
        F: FnMut(&mut dyn Visitor) + 'static,
    {
        Self {
            executor: Box::new(executor),
        }
    }
}

impl MarkingConstraintTrait for SimpleMarkingConstraint {
    fn execute(&mut self, visitor: &mut dyn Visitor) {
        (self.executor)(visitor);
    }
}

pub struct MarkingConstraintSet {
    unexecuted_roots: BitVec,
    unexecuted_outgrowths: BitVec,
    set: Vec<*mut MarkingConstraint>,
    ordered: Vec<*mut MarkingConstraint>,
    outgrowths: Vec<*mut MarkingConstraint>,
    iteration: usize,
}

impl MarkingConstraintSet {
    pub fn new() -> Self {
        Self {
            unexecuted_roots: BitVec::new(),
            unexecuted_outgrowths: BitVec::new(),
            set: Vec::new(),
            ordered: Vec::new(),
            outgrowths: Vec::new(),
            iteration: 1
        }
    }

    pub fn did_start_marking(&mut self) {
        self.unexecuted_outgrowths.clear();
        self.unexecuted_roots.clear();

        self.unexecuted_outgrowths.resize(self.set.len(), false);
        self.unexecuted_roots.resize(self.set.len(), false);

        for constraint in self.set.iter().copied() {
            unsafe {
                match (*constraint).volatility() {
                    ConstraintVolatility::GreyedByExecution => {
                        self.unexecuted_roots.set((*constraint).index(), true);
                    }

                    ConstraintVolatility::GreyedByMarking => {
                        self.unexecuted_outgrowths.set((*constraint).index(), true);
                    }

                    ConstraintVolatility::SeldomGreyed => {}
                }
            }
        }
        self.iteration = 1;
    }

    pub fn add_simple(&mut self, abbreviated_name: &str, name: &str, executor: impl FnMut(&mut dyn Visitor), volatility: ConstraintVolatility) {
        self.add(abbreviated_name, name, SimpleMarkingConstraint::new(executor), volatility);
    }

    pub fn add(&mut self, abbreviated_name: &str, name: &str, constraint: impl MarkingConstraintTrait + 'static, volatility: ConstraintVolatility) {
        let constraint = Box::into_raw(Box::new(MarkingConstraint::new(abbreviated_name, name, volatility, constraint)));
        unsafe { 
            (*constraint).index = self.set.len();
            self.ordered.push(constraint);

            if volatility == ConstraintVolatility::GreyedByMarking {
                self.outgrowths.push(constraint);
            }
            self.set.push(constraint);

        }
    }

    pub fn execute_all_synchronously(&mut self, visitor: &mut dyn Visitor) {
        for constraint in self.set.iter().copied() {
            unsafe {
                (*constraint).execute_synchronously(visitor);
            }
        }
    }

    fn execute_convergence_impl(&mut self, visitor: &mut dyn Visitor) -> bool {
        let mut solver = MarkingConstraintSolver::new(visitor, self);

        let mut iteration = solver.set.iteration;
        solver.set.iteration += 1;

        if iteration == 1 {
            solver.drain(&solver.set.unexecuted_roots);
            return false;
        }

        if iteration == 2 {
            solver.drain(&solver.set.unexecuted_outgrowths);
            return false;
        }

        solver.set.ordered.sort_by(|a,b| unsafe {
            let volatility_score = |constraint: *mut MarkingConstraint| {
                ((*constraint).volatility() == ConstraintVolatility::GreyedByMarking) as usize
            };

            let a_volatility_score = volatility_score(*a);
            let b_volatility_score = volatility_score(*b);
            
            if a_volatility_score != b_volatility_score {
                return b_volatility_score.cmp(&a_volatility_score);
            }

            let a_work_estimate = (**a).work_estimate(visitor);
            let b_work_estimate = (**b).work_estimate(visitor);

            if a_work_estimate != b_work_estimate 
            {
                return a_work_estimate.partial_cmp(&b_work_estimate).unwrap_or(std::cmp::Ordering::Greater);
            }


            ((**a).volatility() as usize).cmp(&((**b).volatility() as usize))
        });

        solver.converge();

        !solver.did_visit_something()
    }

    pub fn iteration(&self) -> usize {
        self.iteration
    }
}

pub struct MarkingConstraintSolver<'a> {
    main_visitor: &'a mut dyn Visitor,
    set: &'a mut MarkingConstraintSet,
    executed: BitVec,
    to_execute_sequentially: Vec<usize>,
    visit_counters: Vec<VisitCounter<'a>>,
    pick_next_is_still_active: bool
}
enum Preference {
    ParallelWorkFirst,
    NextConstraintFirst,
    NoLockingNecessary
}

impl<'a> MarkingConstraintSolver<'a> {
    pub fn new(visitor: &'a mut dyn Visitor, set: &'a mut MarkingConstraintSet) -> Self {
        let mut bvec = BitVec::with_capacity(set.set.len());
        bvec.resize(set.set.len(), false);
        Self {
            main_visitor: visitor,
            set,
            executed: bvec,
            to_execute_sequentially: Vec::new(),
            visit_counters: Vec::new(),
            pick_next_is_still_active: false
        }
    }

    pub fn drain(&mut self, uenxecuted: &BitVec) {
        for (i,item) in uenxecuted.iter().enumerate() {
            if !*item {
                unsafe {
                    self.execute(&mut *self.set.set[i]);
                }
            }
        }
    }

    pub fn did_visit_something(&self) -> bool {
        for c in self.visit_counters.iter() {
            if c.count() != 0 {
                return true;
            }
        }

        false
    }

    pub fn execute(&mut self, constraint: &mut MarkingConstraint) {
        if self.executed[constraint.index()] {
            return;
        }

        constraint.prepare_to_execute(self.main_visitor);
        constraint.execute(self.main_visitor);
        self.executed[constraint.index()] = true;
    }


    pub fn converge(&mut self) {
        if self.did_visit_something() {
            return;
        }

        if self.set.ordered.is_empty() {
            return;
        }

        let mut index = 0;
        unsafe {
            for constraint in self.set.ordered.iter().copied() {
                self.execute(&mut *constraint);
            }
        }
    }
}
