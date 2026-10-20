//! Small filtering helpers (the scipy.signal pieces the port needs):
//! `lfilter` / `sosfilt` with state, FFT convolution / correlation and
//! Hann windows.  All in f64, like scipy.

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

/// Direct form II transposed IIR (`scipy.signal.lfilter`), coefficients
/// normalised by `a[0]`.  `zi` (length `max(len(a), len(b)) - 1`) carries the
/// state across calls.
pub fn lfilter_into(b: &[f64], a: &[f64], x: &[f64], zi: &mut [f64], out: &mut [f64]) {
    let m = b.len().max(a.len());
    assert_eq!(zi.len(), m - 1, "zi length");
    assert_eq!(x.len(), out.len());
    let a0 = a[0];
    let bn: Vec<f64> = (0..m)
        .map(|i| b.get(i).copied().unwrap_or(0.0) / a0)
        .collect();
    let an: Vec<f64> = (0..m)
        .map(|i| a.get(i).copied().unwrap_or(0.0) / a0)
        .collect();
    for (o, &v) in out.iter_mut().zip(x) {
        let y = bn[0] * v + if m > 1 { zi[0] } else { 0.0 };
        for k in 1..m {
            let next = if k < m - 1 { zi[k] } else { 0.0 };
            zi[k - 1] = bn[k] * v + next - an[k] * y;
        }
        *o = y;
    }
}

/// `lfilter(b, a, x)` from zero state.
pub fn lfilter(b: &[f64], a: &[f64], x: &[f64]) -> Vec<f64> {
    let mut zi = vec![0.0; b.len().max(a.len()) - 1];
    let mut out = vec![0.0; x.len()];
    lfilter_into(b, a, x, &mut zi, &mut out);
    out
}

/// Cascade of second-order sections (`scipy.signal.sosfilt`); `sos` is a
/// flat `[b0 b1 b2 a0 a1 a2]*` list, `zi` holds 2 values per section.
/// Allocation free, so it can run per frame.
#[derive(Clone, Debug)]
pub struct Sos {
    sos: Vec<[f64; 6]>,
    zi: Vec<[f64; 2]>,
}

impl Sos {
    pub fn new(flat: &[f64]) -> Self {
        assert_eq!(flat.len() % 6, 0, "sos rows have 6 values");
        let sos: Vec<[f64; 6]> = flat
            .chunks_exact(6)
            .map(|c| {
                let a0 = c[3];
                [c[0] / a0, c[1] / a0, c[2] / a0, 1.0, c[4] / a0, c[5] / a0]
            })
            .collect();
        let zi = vec![[0.0; 2]; sos.len()];
        Self { sos, zi }
    }

    pub fn reset(&mut self) {
        self.zi.iter_mut().for_each(|z| *z = [0.0; 2]);
    }

    pub fn process_sample(&mut self, x: f64) -> f64 {
        let mut v = x;
        for (s, z) in self.sos.iter().zip(self.zi.iter_mut()) {
            let y = s[0] * v + z[0];
            z[0] = s[1] * v - s[4] * y + z[1];
            z[1] = s[2] * v - s[5] * y;
            v = y;
        }
        v
    }

    pub fn process(&mut self, x: &[f64]) -> Vec<f64> {
        x.iter().map(|&v| self.process_sample(v)).collect()
    }
}

/// Population standard deviation (numpy `std`, ddof=0).
pub fn std(x: &[f64]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    let n = x.len() as f64;
    let m = x.iter().sum::<f64>() / n;
    (x.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / n).sqrt()
}

/// Periodic (`fftbins=True`, as used by welch / stft) or symmetric Hann.
pub fn hann(n: usize, periodic: bool) -> Vec<f64> {
    if n == 1 {
        return vec![1.0];
    }
    let d = if periodic { n } else { n - 1 } as f64;
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / d).cos())
        .collect()
}

/// Real FFT of `x` zero-padded to `nfft` (`numpy.fft.rfft(x, nfft)`).
pub fn rfft(x: &[f64], nfft: usize) -> Vec<Complex<f64>> {
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(nfft);
    let mut inp = vec![0.0; nfft];
    let k = x.len().min(nfft);
    inp[..k].copy_from_slice(&x[..k]);
    let mut out = fft.make_output_vec();
    // lengths are right by construction
    fft.process(&mut inp, &mut out).expect("rfft");
    out
}

/// Inverse real FFT normalised by `1/nfft` (`numpy.fft.irfft`); imaginary
/// parts of the DC / Nyquist bins are ignored like numpy does.
pub fn irfft(spec: &[Complex<f64>], nfft: usize) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(nfft);
    let mut s = spec.to_vec();
    s[0].im = 0.0;
    if nfft & 1 == 0 {
        s[nfft / 2].im = 0.0;
    }
    let mut out = ifft.make_output_vec();
    ifft.process(&mut s, &mut out).expect("irfft");
    let g = 1.0 / nfft as f64;
    out.iter_mut().for_each(|v| *v *= g);
    out
}

/// `|rfft(x)|^2`.
pub fn rfft_power(x: &[f64]) -> Vec<f64> {
    rfft(x, x.len()).iter().map(|c| c.norm_sqr()).collect()
}

/// Full linear convolution via FFT (`scipy.signal.fftconvolve`).
pub fn fft_convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let n = a.len() + b.len() - 1;
    let nfft = n.next_power_of_two();
    let fa = rfft(a, nfft);
    let fb = rfft(b, nfft);
    let prod: Vec<Complex<f64>> = fa.iter().zip(&fb).map(|(x, y)| x * y).collect();
    let mut out = irfft(&prod, nfft);
    out.truncate(n);
    out
}

/// Cross-correlation `c[lag] = sum_n b[n + lag] * a[n]` for
/// `lag in -max_lag..=max_lag` (the centre of `correlate(b, a, 'full')`).
pub fn xcorr_lags(a: &[f64], b: &[f64], max_lag: usize) -> Vec<f64> {
    let nfft = (a.len() + b.len()).next_power_of_two().max(2 * max_lag + 2);
    let fa = rfft(a, nfft);
    let fb = rfft(b, nfft);
    let prod: Vec<Complex<f64>> = fb.iter().zip(&fa).map(|(x, y)| x * y.conj()).collect();
    let c = irfft(&prod, nfft);
    (-(max_lag as i64)..=max_lag as i64)
        .map(|lag| {
            // lags outside the overlap are zero in 'full' mode
            if lag >= b.len() as i64 || -lag >= a.len() as i64 {
                0.0
            } else {
                c[lag.rem_euclid(nfft as i64) as usize]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfilter_matches_direct_recursion_and_streams() {
        let b = [0.2, 0.3, -0.1];
        let a = [1.5, -0.6, 0.2];
        let x: Vec<f64> = (0..200).map(|i| ((i * 37 % 17) as f64) - 8.0).collect();
        let y = lfilter(&b, &a, &x);
        for n in 0..x.len() {
            let mut v = 0.0;
            for k in 0..3 {
                if n >= k {
                    v += b[k] * x[n - k];
                }
                if k > 0 && n >= k {
                    v -= a[k] * y[n - k];
                }
            }
            assert!((v / a[0] - y[n]).abs() < 1e-9);
        }
        let mut zi = vec![0.0; 2];
        let mut y2 = vec![0.0; 200];
        lfilter_into(&b, &a, &x[..77], &mut zi, &mut y2[..77]);
        lfilter_into(&b, &a, &x[77..], &mut zi, &mut y2[77..]);
        assert!(y.iter().zip(&y2).all(|(p, q)| (p - q).abs() < 1e-12));
    }

    #[test]
    fn convolution_and_correlation() {
        let a = [1.0, 2.0, 3.0];
        let b = [0.0, 1.0, 0.5];
        let c = fft_convolve(&a, &b);
        let want = [0.0, 1.0, 2.5, 4.0, 1.5];
        assert!(c.iter().zip(want).all(|(p, q)| (p - q).abs() < 1e-12));
        let x: Vec<f64> = (0..100).map(|i| ((i * i) % 13) as f64).collect();
        let mut y = vec![0.0; 5];
        y.extend_from_slice(&x[..95]);
        let cc = xcorr_lags(&x, &y, 10);
        let k = cc
            .iter()
            .enumerate()
            .max_by(|p, q| p.1.total_cmp(q.1))
            .unwrap()
            .0;
        assert_eq!(k as i64 - 10, 5);
    }
}
