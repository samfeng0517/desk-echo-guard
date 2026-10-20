//! Integer-sample streaming delay (`buffers.DelayLine`).

/// Ring buffer, so a block costs O(block) rather than O(delay) (the Siri
/// guard delays by a whole second).
pub struct DelayLine {
    buf: Vec<f32>,
    w: usize,
}

impl DelayLine {
    pub fn new(delay: usize) -> Self {
        Self {
            buf: vec![0.0; delay],
            w: 0,
        }
    }

    pub fn delay(&self) -> usize {
        self.buf.len()
    }

    /// `out[i]` = input from `delay` samples ago; `out.len() == x.len()`.
    pub fn process(&mut self, x: &[f32], out: &mut [f32]) {
        assert_eq!(x.len(), out.len());
        let d = self.buf.len();
        if d == 0 {
            out.copy_from_slice(x);
            return;
        }
        for (o, &v) in out.iter_mut().zip(x) {
            *o = std::mem::replace(&mut self.buf[self.w], v);
            self.w += 1;
            if self.w == d {
                self.w = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_across_block_sizes() {
        let x: Vec<f32> = (0..5000).map(|i| i as f32).collect();
        for delay in [0usize, 1, 7, 480, 1000] {
            let mut d = DelayLine::new(delay);
            let mut y = vec![0.0; x.len()];
            let sizes = [480usize, 1, 333, 2000, 7];
            let (mut i, mut k) = (0, 0);
            while i < x.len() {
                let n = sizes[k % sizes.len()].min(x.len() - i);
                d.process(&x[i..i + n], &mut y[i..i + n]);
                i += n;
                k += 1;
            }
            for (j, v) in y.iter().enumerate() {
                let want = if j < delay { 0.0 } else { (j - delay) as f32 };
                assert_eq!(*v, want, "delay {delay} sample {j}");
            }
        }
    }
}
