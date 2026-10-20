//! Objective measures used by the offline tests and the self-test
//! (`metrics.py`).  Spectral estimates follow scipy's defaults (periodic
//! Hann window, `welch` with constant detrend and density scaling, `stft`
//! with zero boundary padding and spectrum scaling).

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

use crate::filters::{hann, xcorr_lags};
use crate::FS;

pub fn db(x: f64) -> f64 {
    10.0 * x.max(1e-20).log10()
}

/// Mean power.
pub fn energy(x: &[f32]) -> f64 {
    if x.is_empty() {
        return f64::NAN;
    }
    x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64
}

/// Echo return loss enhancement: `10 log10(E[mic^2] / E[out^2])` over a
/// far-end-only region (`mask`), optionally skipping the first `skip_s`
/// seconds of that region (convergence).
pub fn erle_db(mic: &[f32], out: &[f32], mask: Option<&[bool]>, skip_s: f64) -> f64 {
    let n = out.len().min(mic.len());
    let mut m: Vec<bool> = match mask {
        Some(m) => (0..n).map(|i| m.get(i).copied().unwrap_or(false)).collect(),
        None => vec![true; n],
    };
    if skip_s > 0.0 {
        let first = m.iter().position(|&v| v).unwrap_or(0);
        let end = (first + (skip_s * FS as f64) as usize).min(n);
        m[first..end].fill(false);
    }
    let (mut em, mut eo, mut k) = (0.0, 0.0, 0usize);
    for i in (0..n).filter(|&i| m[i]) {
        em += (mic[i] as f64).powi(2);
        eo += (out[i] as f64).powi(2);
        k += 1;
    }
    if k == 0 {
        return f64::NAN;
    }
    db(em / k as f64) - db(eo / k as f64)
}

/// Lag (samples) by which `b` is delayed relative to `a`.
pub fn best_lag(a: &[f32], b: &[f32], max_lag: usize) -> i64 {
    let a: Vec<f64> = a.iter().map(|&v| v as f64).collect();
    let b: Vec<f64> = b.iter().map(|&v| v as f64).collect();
    let c = xcorr_lags(&a, &b, max_lag);
    let mut best = (0usize, f64::NEG_INFINITY);
    for (i, v) in c.iter().enumerate() {
        if v.abs() > best.1 {
            best = (i, v.abs());
        }
    }
    best.0 as i64 - max_lag as i64
}

/// Remove the processing latency of `test` relative to `reference`.
pub fn align<'a>(reference: &'a [f32], test: &'a [f32], max_lag: usize) -> (&'a [f32], &'a [f32]) {
    let lag = best_lag(reference, test, max_lag);
    let (mut r, mut t) = (reference, test);
    if lag > 0 {
        t = &t[(lag as usize).min(t.len())..];
    } else if lag < 0 {
        r = &r[((-lag) as usize).min(r.len())..];
    }
    let n = r.len().min(t.len());
    (&r[..n], &t[..n])
}

/// SNR of `test` against `reference` after optional integer alignment and
/// an optimal scalar gain (so plain level changes are not counted).
pub fn snr_db(reference: &[f32], test: &[f32], align_max: usize) -> f64 {
    let (r, t) = if align_max > 0 {
        align(reference, test, align_max)
    } else {
        let n = reference.len().min(test.len());
        (&reference[..n], &test[..n])
    };
    let dot = |x: &[f32], y: &[f32]| -> f64 {
        x.iter().zip(y).map(|(&p, &q)| p as f64 * q as f64).sum()
    };
    let g = dot(r, t) / dot(r, r).max(1e-20);
    let (mut es, mut ee) = (0.0, 0.0);
    for (&p, &q) in r.iter().zip(t) {
        let s = g * p as f64;
        es += s * s;
        ee += (q as f64 - s).powi(2);
    }
    db(es) - db(ee)
}

/// `scipy.signal.welch(x, fs, nperseg)` -> (freqs, psd).
pub fn welch(x: &[f32], nperseg: usize) -> (Vec<f64>, Vec<f64>) {
    let fs = FS as f64;
    let nper = nperseg.min(x.len()).max(1);
    let nover = nper / 2;
    let step = nper - nover;
    let win = hann(nper, true);
    let scale = 1.0 / (fs * win.iter().map(|w| w * w).sum::<f64>());
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(nper);
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let nbins = spec.len();
    let mut psd = vec![0.0; nbins];
    let nseg = if x.len() >= nper { (x.len() - nover) / step } else { 0 };
    for s in 0..nseg {
        let seg = &x[s * step..s * step + nper];
        let mean = seg.iter().map(|&v| v as f64).sum::<f64>() / nper as f64;
        for ((d, &v), w) in buf.iter_mut().zip(seg).zip(&win) {
            *d = (v as f64 - mean) * w;
        }
        // sizes match the plan by construction
        fft.process(&mut buf, &mut spec).expect("fft");
        for (p, c) in psd.iter_mut().zip(&spec) {
            *p += c.norm_sqr() * scale;
        }
    }
    let last = if nper % 2 == 0 { nbins - 1 } else { nbins };
    for (k, p) in psd.iter_mut().enumerate() {
        *p /= nseg.max(1) as f64;
        if k > 0 && k < last {
            *p *= 2.0;
        }
    }
    let freqs = (0..nbins).map(|k| k as f64 * fs / nper as f64).collect();
    (freqs, psd)
}

/// Energy in `[lo, hi)` Hz from a Welch PSD (nperseg 4096), dB.
pub fn band_energy_db(x: &[f32], lo: f64, hi: f64) -> f64 {
    let (f, p) = welch(x, 4096);
    db(f.iter().zip(&p).filter(|(&f, _)| f >= lo && f < hi).map(|(_, &p)| p).sum())
}

/// `scipy.signal.stft(x, fs, nperseg)` (noverlap = nperseg/2, zero boundary,
/// padded) -> (freqs, frames), `frames[t][bin]`.
pub fn stft(x: &[f32], nperseg: usize) -> (Vec<f64>, Vec<Vec<Complex<f64>>>) {
    let fs = FS as f64;
    let half = nperseg / 2;
    let step = nperseg - half;
    let win = hann(nperseg, true);
    let scale = 1.0 / win.iter().sum::<f64>();
    let mut ext = vec![0.0f64; x.len() + 2 * half];
    for (d, &v) in ext[half..].iter_mut().zip(x) {
        *d = v as f64;
    }
    let len = ext.len();
    let nadd = ((nperseg as i64 - len as i64).rem_euclid(step as i64) as usize) % nperseg;
    ext.resize(len + nadd, 0.0);
    let nseg = (ext.len() - half) / step;
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(nperseg);
    let mut buf = fft.make_input_vec();
    let frames = (0..nseg)
        .map(|s| {
            for ((d, &v), w) in buf.iter_mut().zip(&ext[s * step..s * step + nperseg]).zip(&win) {
                *d = v * w;
            }
            let mut spec = fft.make_output_vec();
            fft.process(&mut buf, &mut spec).expect("fft");
            spec.iter_mut().for_each(|c| *c *= scale);
            spec
        })
        .collect();
    let freqs = (0..nperseg / 2 + 1).map(|k| k as f64 * fs / nperseg as f64).collect();
    (freqs, frames)
}

/// Mean over STFT frames of RMS(dB difference) between spectra, in dB,
/// over `[lo, hi]` Hz (`hi = None` -> fs/2).
pub fn log_spectral_distance(a: &[f32], b: &[f32], lo: f64, hi: Option<f64>) -> f64 {
    let hi = hi.unwrap_or(FS as f64 / 2.0);
    let n = a.len().min(b.len());
    let (f, fa) = stft(&a[..n], 1024);
    let (_, fb) = stft(&b[..n], 1024);
    let sel: Vec<usize> = (0..f.len()).filter(|&k| f[k] >= lo && f[k] <= hi).collect();
    let pow = |frames: &[Vec<Complex<f64>>]| -> Vec<Vec<f64>> {
        frames
            .iter()
            .map(|fr| sel.iter().map(|&k| fr[k].norm_sqr() + 1e-10).collect())
            .collect()
    };
    let (pa, pb) = (pow(&fa), pow(&fb));
    let sums: Vec<f64> = pa.iter().map(|fr| fr.iter().sum()).collect();
    let mx = sums.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let (mut acc, mut cnt) = (0.0, 0usize);
    for (t, s) in sums.iter().enumerate() {
        if *s > 1e-6 * mx {
            let ms: f64 = pa[t]
                .iter()
                .zip(&pb[t])
                .map(|(p, q)| (10.0 * (p / q).log10()).powi(2))
                .sum::<f64>()
                / sel.len() as f64;
            acc += ms.sqrt();
            cnt += 1;
        }
    }
    if cnt == 0 {
        f64::NAN
    } else {
        acc / cnt as f64
    }
}

fn mean_std(x: &[f64]) -> (f64, f64) {
    let n = x.len() as f64;
    let m = x.iter().sum::<f64>() / n;
    (m, (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / n).sqrt())
}

/// Cheap intelligibility proxy (STOI-like): mean correlation of 1/3-octave
/// band envelopes (150 Hz .. 8 kHz) between clean and processed speech, over
/// 384 ms segments.  1.0 = identical envelopes.
pub fn envelope_correlation(reference: &[f32], test: &[f32]) -> f64 {
    let n = reference.len().min(test.len());
    let (f, r) = stft(&reference[..n], 1024);
    let (_, t) = stft(&test[..n], 1024);
    let seg = 36; // frames of 10.7 ms -> ~384 ms
    let mut cs = Vec::new();
    for i in 0..18 {
        let fc = 150.0 * 2f64.powf(i as f64 / 3.0);
        if fc >= 8000.0 {
            continue;
        }
        let (flo, fhi) = (fc * 2f64.powf(-1.0 / 6.0), fc * 2f64.powf(1.0 / 6.0));
        let sel: Vec<usize> = (0..f.len()).filter(|&k| f[k] >= flo && f[k] < fhi).collect();
        let env = |frames: &[Vec<Complex<f64>>]| -> Vec<f64> {
            frames
                .iter()
                .map(|fr| sel.iter().map(|&k| fr[k].norm_sqr()).sum::<f64>().sqrt())
                .collect()
        };
        let (er, et) = (env(&r), env(&t));
        let ermax = er.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut s = 0;
        while s + seg < er.len() {
            let (a, b) = (&er[s..s + seg], &et[s..s + seg]);
            s += seg / 2;
            let (ma, sa) = mean_std(a);
            let amax = a.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            if sa < 1e-6 || amax < 1e-4 * ermax {
                continue;
            }
            let (mb, sb) = mean_std(b);
            if sb < 1e-9 {
                cs.push(0.0);
                continue;
            }
            let cov = a.iter().zip(b).map(|(p, q)| (p - ma) * (q - mb)).sum::<f64>() / seg as f64;
            cs.push(cov / (sa * sb));
        }
    }
    if cs.is_empty() {
        0.0
    } else {
        cs.iter().sum::<f64>() / cs.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same deterministic signals as the scipy reference script.
    fn signals() -> (Vec<f32>, Vec<f32>) {
        let fs = FS as f64;
        let tau = 2.0 * std::f64::consts::PI;
        let x: Vec<f32> = (0..50000)
            .map(|k| {
                let k = k as f64;
                ((tau * 440.0 * k / fs).sin()
                    + 0.5 * (tau * 5000.0 * k / fs + 0.3).sin() * (1.0 + (tau * 3.0 * k / fs).sin())
                    + 0.2 * (tau * (200.0 + 0.05 * k) * k / fs).sin()) as f32
            })
            .collect();
        let y: Vec<f32> = x
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                let k = k as f64;
                (0.8 * v as f64 + 0.1 * (tau * 7000.0 * k / fs).sin() * (tau * 2.0 * k / fs).cos())
                    as f32
            })
            .collect();
        (x, y)
    }

    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} vs {b}");
    }

    #[test]
    fn match_scipy_reference_numbers() {
        let (x, y) = signals();
        close(band_energy_db(&x, 3800.0, 4200.0), -38.36833408287208, 1e-4);
        close(band_energy_db(&x, 4900.0, 5100.0), -17.933837787173065, 1e-4);
        // shorter than nperseg -> one segment of the whole signal
        close(band_energy_db(&y[..3000], 100.0, 1000.0), -16.77938790675448, 1e-4);
        // numpy computes these two in float32
        close(log_spectral_distance(&x, &y, 0.0, None), 6.18387508392334, 2e-3);
        close(log_spectral_distance(&x, &y, 100.0, Some(4000.0)), 1.3485546112060547, 2e-3);
        close(envelope_correlation(&x, &y), 0.9517404477661214, 1e-4);
        close(snr_db(&x, &y, 0), 22.472237788499392, 1e-6);
        let mut yd = vec![0.0f32; 37];
        yd.extend_from_slice(&y);
        close(snr_db(&x, &yd, 100), 22.472237788499392, 1e-6);
        assert_eq!(best_lag(&x, &yd[..50000], 100), 37);
        let mut xd = vec![0.0f32; 20];
        xd.extend_from_slice(&x);
        assert_eq!(best_lag(&xd, &y, 100), -20);
        let (a, b) = align(&x, &yd, 100);
        assert_eq!(a.len(), b.len());
        assert_eq!(b[0], y[0]);
    }

    #[test]
    fn erle_and_db() {
        assert_eq!(db(0.0), -200.0);
        close(db(100.0), 20.0, 1e-12);
        let mic = vec![1.0f32; 48000 * 4];
        let mut out = vec![0.1f32; 48000 * 4];
        out[..48000].fill(1.0); // not converged in the first second
        let mask: Vec<bool> = (0..mic.len()).map(|i| i < 3 * 48000).collect();
        close(erle_db(&mic, &out, Some(&mask), 1.0), 20.0, 1e-9);
        assert!(erle_db(&mic, &out, Some(&mask), 0.0) < 20.0);
        close(erle_db(&mic, &mic, None, 0.0), 0.0, 1e-12);
    }
}
