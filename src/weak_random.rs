pub struct WeakRandom 
{
    seed: usize,
    low: u64,
    high: u64 
}

impl WeakRandom {
    fn advance(&mut self) -> u64 {
        let x = self.low;
        let y = self.high;
        self.low = y;
        self.high = Self::next_state(x, y);
        self.high + self.low
    }

    pub fn next_state(mut x: u64, y: u64) -> u64 {
        x ^= x << 23;
        x ^= x >> 17;
        x ^= y ^ (y >> 26);
        x
    }

    pub fn generate(&self, mut seed: usize) -> u64 {
        if seed == 0 {
            seed = 1;
        }

        let low = seed as u64;
        let high = seed as u64;
        let high = Self::next_state(low, high);
        low + high 
    }

    pub fn get_u64(&mut self) -> u64 {
        self.advance()
    }

    pub fn new(seed: Option<usize>) -> Self {
        let mut this = Self {
            seed: 0,
            high: 0,
            low: 0
        };
        this.set_seed(seed.unwrap_or_else(|| {
            rand::random()
        }));

        this
    }

    pub fn set_seed(&mut self, mut seed: usize) {
        self.seed = seed;

        if seed == 0 {
            seed = 1;
        }

        self.low = seed as _;
        self.high = seed as _;
        self.advance();
    }
}