//! WAV in / out for the offline `--sim-*` commands (`cli._read_wav`,
//! `cli._write_wav`): any PCM / float wav in (mixed to mono, resampled to
//! 48 kHz like `scipy.signal.resample_poly`), 16-bit mono 48 kHz out.

use std::path::Path;

use anyhow::{Context, Result};
use echoguard_dsp::FS;

/// Read a wav as mono f32 at [`FS`].
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut r =
        hound::WavReader::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let spec = r.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f64;
            r.samples::<i32>()
                .map(|v| v.map(|v| (v as f64 * scale) as f32))
                .collect::<Result<_, _>>()?
        }
    };
    let ch = usize::from(spec.channels.max(1));
    let mono: Vec<f32> = if ch == 1 {
        interleaved
    } else {
        interleaved
            .chunks_exact(ch)
            .map(|f| (f.iter().map(|&v| v as f64).sum::<f64>() / ch as f64) as f32)
            .collect()
    };
    let sr = spec.sample_rate as usize;
    if sr == FS || sr == 0 {
        return Ok(mono);
    }
    let g = gcd(sr, FS);
    Ok(resample_poly(&mono, FS / g, sr / g))
}

/// Write mono 16-bit at [`FS`] (clipped, truncated like numpy's astype).
pub fn write_wav(path: &Path, x: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: FS as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)
        .with_context(|| format!("cannot write {}", path.display()))?;
    for &v in x {
        w.write_sample((v * 32767.0).clamp(-32768.0, 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Modified Bessel function of the first kind, order 0 (Kaiser window).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, q) = (1.0, 1.0, x * x / 4.0);
    for k in 1..200 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// `scipy.signal.resample_poly(x, up, down)`: Kaiser (beta 5) windowed-sinc
/// FIR of `20 * max(up, down) + 1` taps, zero-phase, output length
/// `ceil(len * up / down)`.  Only the needed polyphase taps are evaluated.
pub fn resample_poly(x: &[f32], up: usize, down: usize) -> Vec<f32> {
    if up == down {
        return x.to_vec();
    }
    let max_rate = up.max(down);
    let fc = 1.0 / max_rate as f64;
    let half = 10 * max_rate;
    let n_taps = 2 * half + 1;
    let alpha = half as f64;
    let beta = 5.0;
    let i0b = bessel_i0(beta);
    let mut h: Vec<f64> = (0..n_taps)
        .map(|i| {
            let m = i as f64 - alpha;
            let t = fc * m;
            let sinc = if t == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
            };
            let r = m / alpha;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            fc * sinc * w
        })
        .collect();
    // unity gain at DC (firwin scale=True), times `up` for the zero stuffing
    let sum: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v *= up as f64 / sum);

    let n_in = x.len();
    let n_out = (n_in * up).div_ceil(down);
    (0..n_out)
        .map(|k| {
            // y[k] = sum_n x[n] h[k*down + half - n*up]
            let t = k * down + half;
            let n_lo = t.saturating_sub(n_taps - 1).div_ceil(up);
            let n_hi = (t / up).min(n_in.saturating_sub(1));
            let mut acc = 0.0f64;
            let mut n = n_lo;
            while n <= n_hi && n < n_in {
                acc += x[n] as f64 * h[t - n * up];
                n += 1;
            }
            acc as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (0.5 * (2.0 * std::f64::consts::PI * f * i as f64 / sr).sin()) as f32)
            .collect()
    }

    #[test]
    fn resample_44k1_keeps_a_tone() {
        let x = tone(1000.0, 44_100.0, 44_100);
        let y = resample_poly(&x, 160, 147);
        assert_eq!(y.len(), 48_000);
        let want = tone(1000.0, 48_000.0, 48_000);
        // ignore the edges (filter run-in)
        let err = y[1000..47_000]
            .iter()
            .zip(&want[1000..47_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 2e-3, "max error {err}");
    }

    #[test]
    fn downsample_removes_alias_band() {
        // 30 kHz at 96 kHz is above the new Nyquist (24 kHz): must vanish
        let x = tone(30_000.0, 96_000.0, 9600);
        let y = resample_poly(&x, 1, 2);
        assert_eq!(y.len(), 4800);
        let rms = (y[200..4600].iter().map(|v| v * v).sum::<f32>() / 4400.0).sqrt();
        assert!(rms < 1e-3, "rms {rms}");
    }

    #[test]
    fn wav_round_trip_stereo_and_rate() {
        let dir = std::env::temp_dir().join(format!("deg-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 24_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for _ in 0..2400 {
            w.write_sample(16384i16).unwrap();
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let x = read_wav(&p).unwrap();
        assert_eq!(x.len(), 4800);
        assert!((x[2400] - 0.25).abs() < 1e-3, "{}", x[2400]);
        let q = dir.join("o.wav");
        write_wav(&q, &x).unwrap();
        let y = read_wav(&q).unwrap();
        assert_eq!(y.len(), 4800);
        assert!((y[2400] - 0.25).abs() < 1e-3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
