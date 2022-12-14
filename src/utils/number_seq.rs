pub struct TruncatedSeq {
    num: i32,
    sum: f64,
    sum_of_squares: f64,

    davg: f64,
    dvariance: f64,
    alpha: f64,

    sequence: Box<[f64]>,
    next: i32,
}

impl TruncatedSeq {
    pub fn new(length: usize, alpha: f64) -> Self {
        Self {
            num: 0,
            sum: 0.0,
            sum_of_squares: 0.0,
            davg: 0.0,
            dvariance: 0.0,
            alpha,
            sequence: vec![0.0; length].into_boxed_slice(),
            next: 0,
        }
    }

    pub fn total(&self) -> f64 {
        self.num as _
    }

    pub fn add(&mut self, val: f64) {
        if self.num == 0 {
            self.davg = val;
            self.dvariance = 0.0;
        } else {
            let diff = val - self.davg;
            let incr = self.alpha * diff;
            self.davg += incr;
            self.dvariance = (1.0 - self.alpha) * (self.dvariance + diff * incr);
        }

        let old_val = self.sequence[self.next as usize];

        self.sum -= old_val;
        self.sum_of_squares -= old_val * old_val;

        self.sum += val;
        self.sum_of_squares += val * val;

        self.sequence[self.next as usize] = val;
        self.next = (self.next + 1) % self.sequence.len() as i32;

        if self.num < self.sequence.len() as i32 {
            self.num += 1;
        }
    }

    pub fn maximum(&self) -> f64 {
        if self.num == 0 {
            0.0
        } else {
            let mut ret = self.sequence[0];

            for i in 1..self.num {
                let val = self.sequence[i as usize];
                if val > ret {
                    ret = val;
                }
            }

            ret
        }
    }

    pub fn last(&self) -> f64 {
        if self.num == 0 {
            0.0
        } else {
            let last_index = (self.next as usize + self.sequence.len() - 1) % self.sequence.len();
            self.sequence[last_index]
        }
    }

    pub fn oldest(&self) -> f64 {
        if self.num == 0 {
            0.0
        } else if self.num < self.sequence.len() as i32 {
            self.sequence[0]
        } else {
            self.sequence[self.next as usize]
        }
    }

    pub fn predict_next(&self) -> f64 {
        if self.num == 0 {
            return 0.0;
        }

        let num = self.num as f64;

        let mut x_squared_sum = 0.0;
        let mut x_sum = 0.0;
        let mut y_sum = 0.0;
        let mut xy_sum = 0.0;
        let x_avg;
        let y_avg;
        let first =
            (self.next + self.sequence.len() as i32 - self.num) % self.sequence.len() as i32;

        for i in 0..self.num {
            let x = i as f64;
            let y = self.sequence[(first as usize + i as usize) % self.sequence.len()];

            x_squared_sum += x * x;
            x_sum += x;
            y_sum += y;
            xy_sum += x * y;
        }

        x_avg = x_sum / num;
        y_avg = y_sum / num;

        let s_xx = x_squared_sum - x_sum * x_sum / num;
        let s_xy = xy_sum - x_sum * y_sum / num;
        let b1 = s_xy / s_xx;
        let b0 = y_avg - b1 * x_avg;

        return b0 + b1 * num;
    }

    pub fn avg(&self) -> f64 {
        if self.num == 0 {
            0.0
        } else {
            self.sum / self.total()
        }
    }

    pub fn variance(&self) -> f64 {
        if self.num == 0 {
            0.0
        } else {
            let result = self.sum_of_squares / self.total() - self.avg() * self.avg();

            if result < 0.0 {
                return 0.0;
            }

            result
        }
    }

    pub fn sd(&self) -> f64 {
        self.variance().sqrt()
    }

    pub fn davg(&self) -> f64 {
        self.davg
    }

    pub fn dvariance(&self) -> f64 {
        if self.num <= 1 {
            return 0.0;
        }

        if self.dvariance < 0.0 {
            return 0.0;
        }
        self.dvariance
    }

    pub fn dsd(&self) -> f64 {
        self.dvariance().sqrt()
    }
}
