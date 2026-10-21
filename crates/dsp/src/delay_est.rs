//! Reference -> mic delay estimation (GCC-PHAT, `delay.estimate_delay_ms`).

use realfft::num_complex::Complex;

use crate::filters::{irfft, rfft};
use crate::FS;

/// Returns `(delay_ms, confidence)`.
///
/// delay > 0: the echo shows up in `mic` that many ms *after* the same sound
/// appears in `reference` (normal, causal case).  delay < 0: the reference
/// arrives late - delay the mic by -delay (`--mic-delay-ms`).
/// confidence = peak / mean(|cc|), roughly > 5 is trustworthy.
/// Python defaults: `max_delay_ms = 500`, `min_delay_ms = None` (= -max).
pub fn estimate_delay_ms(
    reference: &[f32],
    mic: &[f32],
    max_delay_ms: f64,
    min_delay_ms: Option<f64>,
) -> (f64, f64) {
    let n = reference.len().min(mic.len());
    if n == 0 {
        return (0.0, 0.0);
    }
    let r: Vec<f64> = reference[..n].iter().map(|&v| v as f64).collect();
    let m: Vec<f64> = mic[..n].iter().map(|&v| v as f64).collect();
    let nfft = (2 * n).next_power_of_two();
    let rs = rfft(&r, nfft);
    let ms = rfft(&m, nfft);
    let g: Vec<Complex<f64>> = ms
        .iter()
        .zip(&rs)
        .map(|(a, b)| {
            let v = a * b.conj();
            v / (v.norm() + 1e-12)
        })
        .collect();
    let cc = irfft(&g, nfft);
    let fs = FS as f64;
    let maxlag = (max_delay_ms * fs / 1000.0) as i64;
    let minlag = min_delay_ms.map_or(-maxlag, |v| (v * fs / 1000.0) as i64);
    if minlag > maxlag {
        return (0.0, 0.0);
    }
    let (mut best, mut best_v, mut sum) = (minlag, f64::NEG_INFINITY, 0.0);
    for lag in minlag..=maxlag {
        let v = cc[lag.rem_euclid(nfft as i64) as usize].abs();
        sum += v;
        if v > best_v {
            best_v = v;
            best = lag;
        }
    }
    let mean = sum / (maxlag - minlag + 1) as f64;
    (best as f64 * 1000.0 / fs, best_v / (mean + 1e-12))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testsig::make_scenario;

    #[test]
    fn gcc_phat_finds_echo_delay() {
        for d in [25.0, 120.0] {
            let sc = make_scenario(10.0, d, 0);
            let k = 4 * FS;
            let (est, conf) = estimate_delay_ms(&sc.reference[..k], &sc.mic[..k], 500.0, None);
            // IR direct path is at +3 ms
            assert!((est - (d + 3.0)).abs() < 1.0, "{est}");
            assert!(conf > 5.0, "{conf}");
        }
    }

    #[test]
    fn negative_delay_detected() {
        let sc = make_scenario(6.0, 0.0, 0);
        let mut late = vec![0.0f32; 2400];
        late.extend_from_slice(&sc.reference[..sc.reference.len() - 2400]);
        let k = 4 * FS;
        let (est, _) = estimate_delay_ms(&late[..k], &sc.mic[..k], 500.0, None);
        assert!((est - (3.0 - 50.0)).abs() < 1.0, "{est}");
        // a min_delay_ms of 0 excludes the true (negative) lag
        let (est2, _) = estimate_delay_ms(&late[..k], &sc.mic[..k], 500.0, Some(0.0));
        assert!(est2 >= 0.0);
    }
}
