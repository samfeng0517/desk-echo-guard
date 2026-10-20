//! Partitioned-block frequency-domain adaptive filter (`aec.NlmsAec.process_lowrate`).
//!
//! Overlap-save MDF at 16 kHz: block N = 160 (10 ms), P partitions -> filter
//! length P*N.  Normalised per-bin step with the double-talk-robust factor
//! `mu_k = min(mu_max, |Y_k|^2 / |E_k|^2)` (Valin 2007); a fixed step is
//! blended in until the filter has converged a little.  Gradient constraint
//! (the time-domain gradient's second half is zeroed).  Linear only.

use std::sync::Arc;

use realfft::num_complex::Complex64;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

/// Block length (10 ms at 16 kHz).
pub const NLMS_BLOCK: usize = 160;
const RATE: f64 = 16_000.0;
const MU_MAX: f64 = 0.8;
const MU_INIT: f64 = 0.5;

pub struct Nlms16k {
    n: usize,
    p: usize,
    k: usize,
    /// filter partitions, `w[p*k + bin]`
    w: Vec<Complex64>,
    /// reference spectra ring, age `a` lives in slot `(head + a) % p`
    x: Vec<Complex64>,
    head: usize,
    x_prev: Vec<f64>,
    sxx: Vec<f64>,
    adapted: f64,
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
    // scratch
    t_in: Vec<f64>,
    t_out: Vec<f64>,
    spec: Vec<Complex64>,
    yf: Vec<Complex64>,
    ef: Vec<Complex64>,
    step: Vec<Complex64>,
    e: Vec<f64>,
    fwd_scratch: Vec<Complex64>,
    inv_scratch: Vec<Complex64>,
}

impl Nlms16k {
    /// Filter length `filter_ms` (rounded to whole 10 ms partitions, at least one).
    pub fn new(filter_ms: u32) -> Self {
        let n = NLMS_BLOCK;
        let p = ((filter_ms as f64 * RATE / 1000.0 / n as f64).round_ties_even() as usize).max(1);
        let k = n + 1;
        let mut planner = RealFftPlanner::<f64>::new();
        let fwd = planner.plan_fft_forward(2 * n);
        let inv = planner.plan_fft_inverse(2 * n);
        let fwd_scratch = fwd.make_scratch_vec();
        let inv_scratch = inv.make_scratch_vec();
        let z = Complex64::new(0.0, 0.0);
        Self {
            n,
            p,
            k,
            w: vec![z; p * k],
            x: vec![z; p * k],
            head: 0,
            x_prev: vec![0.0; n],
            sxx: vec![1e-6; k],
            adapted: 0.0,
            fwd,
            inv,
            t_in: vec![0.0; 2 * n],
            t_out: vec![0.0; 2 * n],
            spec: vec![z; k],
            yf: vec![z; k],
            ef: vec![z; k],
            step: vec![z; k],
            e: vec![0.0; n],
            fwd_scratch,
            inv_scratch,
        }
    }

    /// Number of partitions (filter length = `partitions() * 160` samples).
    pub fn partitions(&self) -> usize {
        self.p
    }

    /// rfft of `self.t_in` into `out`.
    fn rfft(&mut self, which: Spec) {
        let out = match which {
            Spec::Tmp => &mut self.spec,
            Spec::Y => &mut self.yf,
            Spec::E => &mut self.ef,
        };
        // lengths are fixed at construction, so this cannot fail
        let _ = self.fwd.process_with_scratch(&mut self.t_in, out, &mut self.fwd_scratch);
    }

    /// Normalised irfft of `self.spec` into `self.t_out` (like `np.fft.irfft`).
    fn irfft(&mut self) {
        // numpy ignores the imaginary parts of the DC / Nyquist bins
        self.spec[0].im = 0.0;
        self.spec[self.k - 1].im = 0.0;
        let _ = self.inv.process_with_scratch(&mut self.spec, &mut self.t_out, &mut self.inv_scratch);
        let s = 1.0 / (2 * self.n) as f64;
        self.t_out.iter_mut().for_each(|v| *v *= s);
    }

    /// One 160-sample block: `out = mic - echo estimate`.
    pub fn process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) {
        let (n, p, k) = (self.n, self.p, self.k);
        assert!(
            mic.len() == n && reference.len() == n && out.len() == n,
            "Nlms16k works on {n}-sample blocks"
        );

        // newest reference spectrum: rfft([x_prev, x])
        self.t_in[..n].copy_from_slice(&self.x_prev);
        for (d, &s) in self.t_in[n..].iter_mut().zip(reference) {
            *d = s as f64;
        }
        for (d, &s) in self.x_prev.iter_mut().zip(reference) {
            *d = s as f64;
        }
        self.rfft(Spec::Tmp);
        self.head = (self.head + p - 1) % p;
        let h = self.head;
        self.x[h * k..(h + 1) * k].copy_from_slice(&self.spec);

        // echo estimate Y = sum_a W[a] * X[a]
        self.spec.iter_mut().for_each(|v| *v = Complex64::new(0.0, 0.0));
        for a in 0..p {
            let slot = (h + a) % p;
            let wr = &self.w[a * k..(a + 1) * k];
            let xr = &self.x[slot * k..(slot + 1) * k];
            for ((acc, &wv), &xv) in self.spec.iter_mut().zip(wr).zip(xr) {
                *acc += wv * xv;
            }
        }
        self.irfft();
        // y = t_out[n..], e = d - y
        let (mut sum_d, mut sum_e) = (0.0, 0.0);
        for i in 0..n {
            let d = mic[i] as f64;
            let e = d - self.t_out[n + i];
            self.e[i] = e;
            out[i] = e as f32;
            sum_d += d * d;
            sum_e += e * e;
        }

        // reference power per bin, summed over partitions
        for b in 0..k {
            let mut s = 0.0;
            for a in 0..p {
                s += self.x[a * k + b].norm_sqr();
            }
            self.sxx[b] = 0.7 * self.sxx[b] + 0.3 * s;
        }

        let sum_x: f64 = reference.iter().map(|&v| (v as f64) * (v as f64)).sum();
        if sum_x <= 1e-7 * n as f64 {
            return; // far end silent: no adaptation
        }
        // Yf = rfft([0, y]), Ef = rfft([0, e])
        self.t_in[..n].iter_mut().for_each(|v| *v = 0.0);
        self.t_in[n..].copy_from_slice(&self.t_out[n..]);
        self.rfft(Spec::Y);
        self.t_in[..n].iter_mut().for_each(|v| *v = 0.0);
        self.t_in[n..].copy_from_slice(&self.e);
        self.rfft(Spec::E);

        if sum_d > 2.0 * sum_e {
            self.adapted = (self.adapted + 0.02).min(1.0);
        }
        let reg = 1e-2 * self.sxx.iter().sum::<f64>() / k as f64 + 1e-8;
        for b in 0..k {
            let pe = self.ef[b].norm_sqr();
            let py = self.yf[b].norm_sqr();
            let mu_dt = MU_MAX.min(py / (pe + 1e-10));
            let mu = self.adapted * mu_dt + (1.0 - self.adapted) * MU_INIT;
            self.step[b] = self.ef[b] * (mu / (self.sxx[b] + reg));
        }

        // constrained gradient per partition
        for a in 0..p {
            let slot = (h + a) % p;
            for b in 0..k {
                self.spec[b] = self.x[slot * k + b].conj() * self.step[b];
            }
            self.irfft();
            self.t_in[..n].copy_from_slice(&self.t_out[..n]);
            self.t_in[n..].iter_mut().for_each(|v| *v = 0.0);
            self.rfft(Spec::Tmp);
            for (wv, &g) in self.w[a * k..(a + 1) * k].iter_mut().zip(&self.spec) {
                *wv += g;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Spec {
    Tmp,
    Y,
    E,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};

    #[test]
    fn partitions_round_like_python() {
        assert_eq!(Nlms16k::new(300).partitions(), 30);
        assert_eq!(Nlms16k::new(25).partitions(), 2); // banker's rounding
        assert_eq!(Nlms16k::new(1).partitions(), 1);
    }

    #[test]
    #[should_panic]
    fn rejects_wrong_block() {
        let mut f = Nlms16k::new(100);
        let mut out = [0.0; 100];
        f.process(&[0.0; 100], &[0.0; 100], &mut out);
    }

    #[test]
    fn converges_on_fir_echo() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(5);
        let n = 16_000 * 6;
        let x: Vec<f32> = (0..n).map(|_| rng.gen_range(-0.3f32..0.3)).collect();
        // echo path: 2 ms delay + short decaying tail
        let mut hpath = vec![0.0f32; 400];
        hpath[32] = 0.5;
        for (i, v) in hpath.iter_mut().enumerate().skip(33) {
            *v = 0.1 * (-(i as f32 - 33.0) / 60.0).exp() * if i % 2 == 0 { 1.0 } else { -1.0 };
        }
        let d: Vec<f32> = (0..n)
            .map(|i| hpath.iter().enumerate().filter(|(j, _)| *j <= i).map(|(j, &h)| h * x[i - j]).sum())
            .collect();
        let mut f = Nlms16k::new(100);
        let mut out = vec![0.0; n];
        for s in (0..n).step_by(NLMS_BLOCK) {
            f.process(&d[s..s + NLMS_BLOCK], &x[s..s + NLMS_BLOCK], &mut out[s..s + NLMS_BLOCK]);
        }
        let tail = n - 16_000 * 2;
        let pd: f64 = d[tail..].iter().map(|&v| (v as f64).powi(2)).sum();
        let pe: f64 = out[tail..].iter().map(|&v| (v as f64).powi(2)).sum();
        let erle = 10.0 * (pd / pe).log10();
        assert!(erle > 25.0, "ERLE {erle:.1} dB");
    }
}
