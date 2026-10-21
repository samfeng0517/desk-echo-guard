//! Linear-phase FIR multi-band-stop for the Siri guard
//! (`siri_guard.NotchConfig` / `PROFILES` / `design_notch_fir` / `NotchBank`).
//!
//! Linear phase means the notched ("wet") signal is sample-aligned with the
//! delayed dry signal, so the guard can cross-fade between them without comb
//! filtering.

use std::sync::Arc;

use anyhow::bail;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::filters::{irfft, rfft};
use crate::FS;

/// Stop-band profiles.  "apple-deep" uses the frequencies reported for WWDC
/// 2026 (depth not public); "deep-wide" (the default) cuts the whole
/// 2.8-6.5 kHz range as deep as apple-deep.
pub const PROFILES: &[&str] = &["deep-wide", "apple-deep", "apple-wide", "formant"];
pub const DEFAULT_PROFILE: &str = "deep-wide";
/// Names from older versions, still accepted in saved settings.
pub const LEGACY_PROFILES: &[(&str, &str)] = &[("apple", DEFAULT_PROFILE)];

fn apple_bands(width: f64) -> Vec<(f64, f64)> {
    [3000.0, 4000.0, 5000.0, 6000.0]
        .iter()
        .map(|c| (c - width / 2.0, c + width / 2.0))
        .collect()
}

fn profile_params(name: &str) -> Option<(Vec<(f64, f64)>, f64)> {
    Some(match name {
        "deep-wide" => (vec![(2800.0, 6500.0)], 70.0),
        "apple-deep" => (apple_bands(400.0), 70.0),
        "apple-wide" => (vec![(2800.0, 6500.0)], 50.0),
        "formant" => {
            let mut b = vec![(1000.0, 3000.0)];
            b.extend(apple_bands(400.0));
            (b, 70.0)
        }
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct NotchConfig {
    /// `(lo_hz, hi_hz)` stop bands.
    pub bands: Vec<(f64, f64)>,
    pub depth_db: f64,
    /// Skirt on each side.
    pub transition_hz: f64,
    /// Odd -> integer group delay (1201 taps = 600 samples = 12.5 ms).
    pub numtaps: usize,
}

impl Default for NotchConfig {
    fn default() -> Self {
        let (bands, depth_db) = profile_params(DEFAULT_PROFILE).expect("default profile");
        Self {
            bands,
            depth_db,
            transition_hz: 150.0,
            numtaps: 1201,
        }
    }
}

impl NotchConfig {
    /// A named profile, optionally with another depth.
    pub fn profile(name: &str, depth_db: Option<f64>) -> anyhow::Result<Self> {
        let name = LEGACY_PROFILES
            .iter()
            .find(|(old, _)| *old == name)
            .map_or(name, |(_, new)| new);
        let Some((bands, depth)) = profile_params(name) else {
            let mut names: Vec<&str> = PROFILES.to_vec();
            names.sort_unstable();
            bail!("unknown notch profile {name:?}; choose from {names:?}");
        };
        Ok(Self {
            bands,
            depth_db: depth_db.unwrap_or(depth),
            ..Self::default()
        })
    }

    /// Group delay in samples.
    pub fn delay(&self) -> usize {
        (self.numtaps.max(1) - 1) / 2
    }
}

/// Desired magnitude (linear) at `f`: the minimum over stop bands of a
/// trapezoid (1 outside, `-depth_db` inside, linear skirts), so overlapping
/// bands merge cleanly.
pub fn notch_target_gain(cfg: &NotchConfig, f: f64) -> f64 {
    let g = 10f64.powf(-cfg.depth_db / 20.0);
    let tr = cfg.transition_hz.max(1.0);
    let mut out = 1.0f64;
    for &(lo, hi) in &cfg.bands {
        let v = if f >= lo && f <= hi {
            g
        } else if f > lo - tr && f < lo {
            1.0 - (1.0 - g) * (f - (lo - tr)) / tr
        } else if f > hi && f < hi + tr {
            g + (1.0 - g) * (f - hi) / tr
        } else {
            1.0
        };
        out = out.min(v);
    }
    out
}

/// Modified Bessel function of the first kind, order 0 (power series; the
/// terms fall off fast for the Kaiser betas used here).
pub fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term, mut k) = (1.0f64, 1.0f64, 1.0f64);
    loop {
        term *= q / (k * k);
        sum += term;
        if term < sum * 1e-17 {
            return sum;
        }
        k += 1.0;
    }
}

/// Symmetric Kaiser window (`get_window(("kaiser", beta), n, fftbins=False)`).
pub fn kaiser(n: usize, beta: f64) -> Vec<f64> {
    if n == 1 {
        return vec![1.0];
    }
    let den = bessel_i0(beta);
    let alpha = (n - 1) as f64 / 2.0;
    (0..n)
        .map(|i| {
            let r = (i as f64 - alpha) / alpha;
            bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / den
        })
        .collect()
}

/// `numpy.interp` (xp increasing, clamped at the ends).
fn interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if x <= xp[0] {
        return fp[0];
    }
    if x >= xp[n - 1] {
        return fp[n - 1];
    }
    // first index with xp[j] > x
    let j = xp.partition_point(|&v| v <= x);
    let (x0, x1) = (xp[j - 1], xp[j]);
    let (y0, y1) = (fp[j - 1], fp[j]);
    let slope = (y1 - y0) / (x1 - x0);
    slope * (x - x0) + y0
}

/// The notch FIR exactly as `siri_guard.design_notch_fir`:
/// `scipy.signal.firwin2(numtaps, linspace(0, fs/2, 4097), target, fs=fs,
/// window=("kaiser", 6.0))` (frequency sampling on a `1 + 2**ceil(log2(numtaps))`
/// point mesh, then windowed).
pub fn design_notch_fir(cfg: &NotchConfig) -> Vec<f64> {
    let numtaps = cfg.numtaps;
    assert!(
        numtaps % 2 == 1,
        "numtaps must be odd (type I filter, integer delay)"
    );
    let nyq = FS as f64 / 2.0;
    let freq: Vec<f64> = (0..4097).map(|i| i as f64 * (nyq / 4096.0)).collect();
    let gain: Vec<f64> = freq.iter().map(|&f| notch_target_gain(cfg, f)).collect();
    let nfreqs = 1 + (1usize << (numtaps as f64).log2().ceil() as u32);
    let nfft = 2 * (nfreqs - 1);
    let half = (numtaps - 1) as f64 / 2.0;
    let spec: Vec<Complex<f64>> = (0..nfreqs)
        .map(|i| {
            // np.linspace(0, nyq, nfreqs)
            let x = if i == nfreqs - 1 {
                nyq
            } else {
                i as f64 * (nyq / (nfreqs - 1) as f64)
            };
            let fx = interp(x, &freq, &gain);
            // phase shift so the first numtaps outputs are the filter
            let ph = -half * std::f64::consts::PI * x / nyq;
            Complex::new(fx * ph.cos(), fx * ph.sin())
        })
        .collect();
    let full = irfft(&spec, nfft);
    let w = kaiser(numtaps, 6.0);
    full[..numtaps].iter().zip(&w).map(|(h, w)| h * w).collect()
}

/// Overlap-save plan for one FFT size (filter spectrum precomputed).
struct Plan {
    nfft: usize,
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
    h: Vec<Complex<f64>>,
    buf: Vec<f64>,
    spec: Vec<Complex<f64>>,
    out: Vec<f64>,
    scratch_f: Vec<Complex<f64>>,
    scratch_i: Vec<Complex<f64>>,
}

/// Streaming linear-phase FIR multi-band-stop (FFT overlap-save).  Any block
/// split gives the same output; the per-block path does not allocate once
/// a block size has been seen.
pub struct NotchBank {
    cfg: NotchConfig,
    h: Vec<f64>,
    /// last `len(h) - 1` input samples
    hist: Vec<f64>,
    plans: Vec<Plan>,
    planner: RealFftPlanner<f64>,
    max_nfft: usize,
}

impl NotchBank {
    pub fn new(cfg: NotchConfig) -> Self {
        let h = design_notch_fir(&cfg);
        let l = h.len();
        Self {
            cfg,
            hist: vec![0.0; l - 1],
            // long inputs are split so the FFT stays small and cache friendly
            max_nfft: (4 * l).next_power_of_two().max(1024),
            h,
            plans: Vec::new(),
            planner: RealFftPlanner::new(),
        }
    }

    pub fn config(&self) -> &NotchConfig {
        &self.cfg
    }

    pub fn taps(&self) -> &[f64] {
        &self.h
    }

    /// Group delay in samples.
    pub fn delay(&self) -> usize {
        (self.h.len() - 1) / 2
    }

    fn plan_index(&mut self, nfft: usize) -> usize {
        if let Some(i) = self.plans.iter().position(|p| p.nfft == nfft) {
            return i;
        }
        let fwd = self.planner.plan_fft_forward(nfft);
        let inv = self.planner.plan_fft_inverse(nfft);
        // scale by 1/nfft once here instead of per block
        let g = 1.0 / nfft as f64;
        let h = rfft(&self.h, nfft).into_iter().map(|c| c * g).collect();
        self.plans.push(Plan {
            nfft,
            buf: fwd.make_input_vec(),
            spec: fwd.make_output_vec(),
            out: inv.make_output_vec(),
            scratch_f: fwd.make_scratch_vec(),
            scratch_i: inv.make_scratch_vec(),
            fwd,
            inv,
            h,
        });
        self.plans.len() - 1
    }

    pub fn process(&mut self, x: &[f32], out: &mut [f32]) {
        assert_eq!(x.len(), out.len());
        let m = self.hist.len(); // L - 1
        let max_block = self.max_nfft - m;
        let mut i = 0;
        while i < x.len() {
            let c = (x.len() - i).min(max_block);
            let nfft = (c + m).next_power_of_two();
            let pi = self.plan_index(nfft);
            let p = &mut self.plans[pi];
            p.buf[..m].copy_from_slice(&self.hist);
            for (d, &v) in p.buf[m..m + c].iter_mut().zip(&x[i..i + c]) {
                *d = v as f64;
            }
            p.buf[m + c..].fill(0.0);
            self.hist.copy_from_slice(&p.buf[c..c + m]);
            // sizes match the plan by construction
            p.fwd
                .process_with_scratch(&mut p.buf, &mut p.spec, &mut p.scratch_f)
                .expect("fft");
            for (s, h) in p.spec.iter_mut().zip(&p.h) {
                *s *= h;
            }
            p.spec[0].im = 0.0;
            if let Some(last) = p.spec.last_mut() {
                last.im = 0.0;
            }
            p.inv
                .process_with_scratch(&mut p.spec, &mut p.out, &mut p.scratch_i)
                .expect("ifft");
            for (o, &v) in out[i..i + c].iter_mut().zip(&p.out[m..m + c]) {
                *o = v as f32;
            }
            i += c;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::synth_speech;

    fn gain_db(h: &[f64], f: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * f / FS as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &v) in h.iter().enumerate() {
            re += v * (w * k as f64).cos();
            im -= v * (w * k as f64).sin();
        }
        20.0 * ((re * re + im * im).sqrt() + 1e-12).log10()
    }

    #[test]
    fn bessel_and_kaiser() {
        // scipy.special.i0(6.0), np.kaiser(5, 6.0)
        assert!((bessel_i0(6.0) - 67.23440697647797).abs() < 1e-10);
        let w = kaiser(5, 6.0);
        let want = [
            0.014873337104763207,
            0.4829556064106269,
            1.0,
            0.4829556064106269,
            0.014873337104763207,
        ];
        for (a, b) in w.iter().zip(want) {
            assert!((a - b).abs() < 1e-12, "{a} vs {b}");
        }
    }

    #[test]
    fn design_matches_scipy_firwin2() {
        let c = NotchConfig {
            bands: vec![(5000.0, 9000.0), (8500.0, 12000.0)],
            depth_db: 40.0,
            transition_hz: 1000.0,
            numtaps: 31,
        };
        let want = [
            0.00028039209417873693,
            0.001300816864743074,
            0.0005054580868585145,
            -0.0002457944432156083,
            0.004324404517033635,
            0.0022049295090836698,
            -0.018280244558281283,
            -0.025260832094483075,
            0.0012690423860097367,
            0.0015134043257311366,
            -0.03371183907315548,
            0.02656344478101479,
            0.18294347990735754,
            0.16013831900286996,
            -0.1374965924715633,
            0.6674218749999999,
        ];
        let h = design_notch_fir(&c);
        assert_eq!(h.len(), 31);
        for (k, w) in want.iter().enumerate() {
            assert!((h[k] - w).abs() < 1e-12, "tap {k}: {} vs {w}", h[k]);
            assert!((h[30 - k] - w).abs() < 1e-12, "tap {}", 30 - k);
        }
        // default profile, a few taps (scipy 1.18)
        let h = design_notch_fir(&NotchConfig::default());
        for (k, w) in [
            (0, 2.5251750059845363e-07),
            (1, 1.2738297315566367e-07),
            (100, -7.31984473114589e-06),
            (300, -1.1484542151000069e-05),
            (550, -0.00022096950134848855),
            (599, -0.1301747552890317),
            (600, 0.8396325361410842),
        ] {
            assert!((h[k] - w).abs() < 1e-12, "tap {k}: {} vs {w}", h[k]);
        }
        let h = design_notch_fir(&NotchConfig::profile("formant", None).unwrap());
        for (k, w) in [
            (0, 4.091919164489445e-07),
            (7, -1.3239382626348216e-07),
            (250, 7.664408981591244e-06),
            (600, 0.8333834956337012),
        ] {
            assert!((h[k] - w).abs() < 1e-12, "tap {k}: {} vs {w}", h[k]);
        }
    }

    #[test]
    fn notch_response_all_profiles() {
        for name in PROFILES {
            let cfg = NotchConfig::profile(name, None).unwrap();
            let h = design_notch_fir(&cfg);
            assert!(h.len() % 2 == 1);
            assert!(h
                .iter()
                .zip(h.iter().rev())
                .all(|(a, b)| (a - b).abs() < 1e-12));
            for &(lo, hi) in &cfg.bands {
                // the window smears the very edges
                for j in 0..5 {
                    let f = lo + 75.0 + (hi - lo - 150.0) * j as f64 / 4.0;
                    let g = gain_db(&h, f);
                    assert!(g < -cfg.depth_db + 3.0, "{name} {f}: {g}");
                }
            }
            // pass band untouched (< 0.5 dB) well away from every stop band
            for f in [200.0, 500.0, 7500.0, 10000.0] {
                if cfg
                    .bands
                    .iter()
                    .all(|&(lo, hi)| !(lo - 300.0 < f && f < hi + 300.0))
                {
                    assert!(gain_db(&h, f).abs() < 0.5, "{name} {f}");
                }
            }
        }
    }

    #[test]
    fn streaming_equals_offline() {
        let x = synth_speech(2.0, 4);
        let mut nb1 = NotchBank::new(NotchConfig::default());
        let mut nb2 = NotchBank::new(NotchConfig::default());
        let mut y_blocks = vec![0.0; x.len()];
        for (xi, yi) in x.chunks(480).zip(y_blocks.chunks_mut(480)) {
            nb1.process(xi, yi);
        }
        let mut y_once = vec![0.0; x.len()];
        nb2.process(&x, &mut y_once);
        for (a, b) in y_blocks.iter().zip(&y_once) {
            assert!((a - b).abs() < 1e-5);
        }
        // and both equal direct convolution
        let h = nb1.taps().to_vec();
        for n in [0usize, 599, 600, 5000, 77777] {
            let v: f64 = (0..h.len())
                .filter(|&k| k <= n)
                .map(|k| h[k] * x[n - k] as f64)
                .sum();
            assert!((v as f32 - y_once[n]).abs() < 1e-6, "{n}");
        }
        // odd block sizes too
        let mut nb3 = NotchBank::new(NotchConfig::default());
        let mut y3 = vec![0.0; x.len()];
        let mut i = 0;
        for k in 0.. {
            if i >= x.len() {
                break;
            }
            let n = [1usize, 7, 333, 2000, 12000][k % 5].min(x.len() - i);
            nb3.process(&x[i..i + n], &mut y3[i..i + n]);
            i += n;
        }
        assert!(y3.iter().zip(&y_once).all(|(a, b)| (a - b).abs() < 1e-5));
    }

    #[test]
    fn legacy_apple_profile_maps_to_default() {
        let a = NotchConfig::profile("apple", None).unwrap();
        assert_eq!(a, NotchConfig::profile("deep-wide", None).unwrap());
        assert_eq!(a, NotchConfig::default());
        assert!(NotchConfig::profile("nope", None).is_err());
        assert_eq!(
            NotchConfig::profile("formant", Some(40.0))
                .unwrap()
                .depth_db,
            40.0
        );
    }
}
