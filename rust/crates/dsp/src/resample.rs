//! Streaming integer-ratio resampler (`resample.IntegerResampler`).

/// `scipy.signal.firwin(numtaps, cutoff)`: Hamming-windowed sinc low-pass,
/// `cutoff` relative to Nyquist, unity gain at DC.
pub fn firwin_lowpass(numtaps: usize, cutoff: f64) -> Vec<f64> {
    let alpha = 0.5 * (numtaps as f64 - 1.0);
    let mut h: Vec<f64> = (0..numtaps)
        .map(|i| {
            let m = i as f64 - alpha;
            let x = cutoff * m;
            let sinc = if x == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
            };
            let win = if numtaps > 1 {
                0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / (numtaps as f64 - 1.0)).cos()
            } else {
                1.0
            };
            cutoff * sinc * win
        })
        .collect();
    let s: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= s);
    h
}

/// Stateful up- or down-sampler by an integer factor.
///
/// `factor > 0`: downsample by `factor`; `factor < 0`: upsample by `-factor`.
/// Linear-phase FIR (32 taps per phase, cutoff 0.9 of the low rate's
/// Nyquist), state kept across calls, so any block split gives the same
/// output (block length must be a multiple of the factor when downsampling).
/// Polyphase: only the kept outputs are computed.
pub struct IntegerResampler {
    down: bool,
    k: usize,
    h: Vec<f64>,
    /// reversed taps, `[m][p]` for upsampling (h[m*k + p])
    taps: Vec<f64>,
    hist: Vec<f64>,
    phase: usize,
}

impl IntegerResampler {
    pub fn new(factor: i32) -> Self {
        Self::with_taps(factor, 32)
    }

    pub fn with_taps(factor: i32, taps_per_phase: usize) -> Self {
        assert!(factor != 0, "factor must be non-zero");
        let down = factor > 0;
        let k = factor.unsigned_abs() as usize;
        let mut h = firwin_lowpass(taps_per_phase * k + 1, 0.9 / k as f64);
        if !down {
            h.iter_mut().for_each(|v| *v *= k as f64);
        }
        let (taps, hist_len) = if down {
            (h.iter().rev().copied().collect(), h.len() - 1)
        } else {
            let m = h.len().div_ceil(k);
            let mut t = vec![0.0; m * k];
            // row r = m-1-j holds h[j*k .. j*k+k] so a forward dot over the
            // history window lines up with the newest sample last
            for j in 0..m {
                for p in 0..k {
                    t[(m - 1 - j) * k + p] = h.get(j * k + p).copied().unwrap_or(0.0);
                }
            }
            (t, m - 1)
        };
        Self {
            down,
            k,
            h,
            taps,
            hist: vec![0.0; hist_len],
            phase: 0,
        }
    }

    /// Group delay in high-rate samples.
    pub fn delay(&self) -> usize {
        (self.h.len() - 1) / 2
    }

    pub fn taps(&self) -> &[f64] {
        &self.h
    }

    pub fn process(&mut self, x: &[f32]) -> Vec<f32> {
        if self.k == 1 {
            return x.to_vec();
        }
        let hl = self.hist.len();
        let mut buf = Vec::with_capacity(hl + x.len());
        buf.extend_from_slice(&self.hist);
        buf.extend(x.iter().map(|&v| v as f64));
        let out = if self.down {
            let l = hl + 1;
            let mut out = Vec::with_capacity(x.len() / self.k + 1);
            let mut i = self.phase;
            while i < x.len() {
                let w = &buf[i..i + l];
                out.push(w.iter().zip(&self.taps).map(|(a, b)| a * b).sum::<f64>() as f32);
                i += self.k;
            }
            self.phase = (self.phase + self.k - x.len() % self.k) % self.k;
            out
        } else {
            let m = hl + 1;
            let k = self.k;
            let mut out = vec![0.0f32; x.len() * k];
            for n in 0..x.len() {
                let w = &buf[n..n + m];
                for p in 0..k {
                    let mut acc = 0.0;
                    for (r, &v) in w.iter().enumerate() {
                        acc += v * self.taps[r * k + p];
                    }
                    out[n * k + p] = acc as f32;
                }
            }
            out
        };
        self.hist.copy_from_slice(&buf[buf.len() - hl..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct-form reference: filter at the high rate, keep every k-th sample
    /// (down) or zero-stuff first (up), like the Python version.
    fn reference(factor: i32, x: &[f32]) -> Vec<f32> {
        let r = IntegerResampler::new(factor);
        let h = r.taps().to_vec();
        let k = factor.unsigned_abs() as usize;
        let hi: Vec<f64> = if factor > 0 {
            x.iter().map(|&v| v as f64).collect()
        } else {
            let mut u = vec![0.0; x.len() * k];
            for (i, &v) in x.iter().enumerate() {
                u[i * k] = v as f64;
            }
            u
        };
        let y: Vec<f64> = (0..hi.len())
            .map(|n| {
                (0..h.len())
                    .filter(|&j| j <= n)
                    .map(|j| h[j] * hi[n - j])
                    .sum()
            })
            .collect();
        if factor > 0 {
            y.iter().step_by(k).map(|&v| v as f32).collect()
        } else {
            y.iter().map(|&v| v as f32).collect()
        }
    }

    #[test]
    fn firwin_matches_scipy() {
        // scipy.signal.firwin(9, 0.3)
        let want = [
            -0.00408637,
            0.00768855,
            0.08926046,
            0.24333123,
            0.32761225,
            0.24333123,
            0.08926046,
            0.00768855,
            -0.00408637,
        ];
        for (a, b) in firwin_lowpass(9, 0.3).iter().zip(want) {
            assert!((a - b).abs() < 1e-7, "{a} vs {b}");
        }
    }

    #[test]
    fn matches_direct_form_for_any_block_split() {
        let x: Vec<f32> = (0..2400)
            .map(|i| ((i * 7919 % 1000) as f32 / 500.0) - 1.0)
            .collect();
        for (factor, sizes) in [
            (3, vec![480usize, 6, 960, 3]),
            (2, vec![2, 480, 10]),
            (-3, vec![160usize, 1, 7, 333]),
        ] {
            let mut r = IntegerResampler::new(factor);
            let (mut y, mut i, mut k) = (Vec::new(), 0, 0);
            while i < x.len() {
                let n = sizes[k % sizes.len()].min(x.len() - i);
                y.extend(r.process(&x[i..i + n]));
                i += n;
                k += 1;
            }
            let want = reference(factor, &x);
            assert_eq!(y.len(), want.len());
            for (a, b) in y.iter().zip(&want) {
                assert!((a - b).abs() < 1e-5, "factor {factor}: {a} vs {b}");
            }
        }
    }
}
