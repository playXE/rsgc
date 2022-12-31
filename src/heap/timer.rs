use std::fmt;
pub struct CollectionStats {
    collections: usize,
    total_pause: f32,
    pauses: Vec<f32>,
}

impl CollectionStats {
    fn new() -> CollectionStats {
        CollectionStats {
            collections: 0,
            total_pause: 0f32,
            pauses: Vec::new(),
        }
    }

    fn add(&mut self, pause: f32) {
        self.collections += 1;
        self.total_pause += pause;
        self.pauses.push(pause);
    }

    fn pause(&self) -> f32 {
        self.total_pause
    }

    fn pauses(&self) -> AllNumbers {
        AllNumbers(self.pauses.clone())
    }

    fn mutator(&self, runtime: f32) -> f32 {
        runtime - self.total_pause
    }

    fn collections(&self) -> usize {
        self.collections
    }

    fn percentage(&self, runtime: f32) -> (f32, f32) {
        let gc_percentage = ((self.total_pause / runtime) * 100.0).round();
        let mutator_percentage = 100.0 - gc_percentage;

        (mutator_percentage, gc_percentage)
    }
}


pub struct AllNumbers(Vec<f32>);

impl fmt::Display for AllNumbers {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[")?;
        let mut first = true;
        for num in &self.0 {
            if !first {
                write!(f, ",")?;
            }
            write!(f, "{:.1}", num)?;
            first = false;
        }
        write!(f, "]")
    }
}

