//! Unit tests that need no test-signal helpers from echoguard-dsp.

use std::time::Instant;

use rand::{Rng, SeedableRng};

use super::*;

/// White-noise far end through a short echo path (`delay` samples + tail).
fn white_echo(secs: f64, delay: usize, seed: u64) -> (Vec<f32>, Vec<f32>) {
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let n = (secs * FS as f64) as usize;
    let x: Vec<f32> = (0..n).map(|_| rng.gen_range(-0.2f32..0.2)).collect();
    let mut h = vec![0.0f32; delay + 1200];
    h[delay] = 0.5;
    for (i, v) in h.iter_mut().enumerate().skip(delay + 1) {
        let t = (i - delay) as f32;
        *v = 0.15 * (-t / 300.0).exp() * rng.gen_range(-1.0f32..1.0);
    }
    let mut mic = vec![0.0f32; n];
    for (i, m) in mic.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for (j, &hv) in h.iter().enumerate().take(i + 1) {
            if hv != 0.0 {
                acc += hv * x[i - j];
            }
        }
        *m = acc + rng.gen_range(-1e-4f32..1e-4);
    }
    (mic, x)
}

/// ERLE over the last `tail_s` seconds.
fn tail_erle(mic: &[f32], out: &[f32], tail_s: f64) -> f64 {
    let n = out.len();
    let s = n - (tail_s * FS as f64) as usize;
    let p = |v: &[f32]| v.iter().map(|&a| (a as f64).powi(2)).sum::<f64>().max(1e-20);
    10.0 * (p(&mic[s..n]) / p(&out[s..n])).log10()
}

fn erle_for(engine: &str, opts: &EngineOptions) -> f64 {
    let (mic, x) = white_echo(8.0, 240, 7);
    let mut p = AecProcessor::new(make_engine(engine, opts).unwrap(), 0.0, 0.0, 0.0);
    let out = p.process_signal(&mic, &x).unwrap();
    let e = tail_erle(&mic, &out, 3.0);
    println!("[metrics] white-noise echo ERLE {engine}: {e:.1} dB");
    e
}

#[test]
fn engine_lists() {
    let av = available_engines();
    assert_eq!(*av.last().unwrap(), "off");
    assert!(av.contains(&"nlms"));
    for e in &av {
        assert!(ENGINES.contains(e));
        assert_eq!(make_engine(e, &EngineOptions::default()).unwrap().name(), *e);
    }
    assert_eq!(make_engine("auto", &EngineOptions::default()).unwrap().name(), av[0]);
    assert!(make_engine("bogus", &EngineOptions::default()).is_err());
}

#[test]
fn default_options() {
    let o = EngineOptions::default();
    assert!(o.noise_suppression && o.high_pass_filter && !o.auto_gain_control);
    assert_eq!(o.filter_ms, 300);
}

#[test]
fn passthrough_is_identity_with_gain_and_clip() {
    let mic: Vec<f32> = (0..FRAME * 3).map(|i| ((i as f32) * 0.01).sin() * 0.9).collect();
    let r = vec![0.3f32; FRAME * 3];
    let mut p = AecProcessor::new(make_engine("off", &EngineOptions::default()).unwrap(), 0.0, 0.0, 0.0);
    assert_eq!(p.engine_name(), "off");
    assert_eq!(p.process_signal(&mic, &r).unwrap(), mic);
    // +6 dB gain clips at 1
    let mut p = AecProcessor::new(Box::new(PassThrough), 0.0, 0.0, 6.0);
    let out = p.process_signal(&mic, &r).unwrap();
    for (o, m) in out.iter().zip(&mic) {
        let want = (m * 10f64.powf(6.0 / 20.0) as f32).clamp(-1.0, 1.0);
        assert!((o - want).abs() < 1e-6);
    }
    assert!(out.iter().any(|&v| v == 1.0));
}

#[test]
fn process_signal_truncates_to_frames() {
    let mut p = AecProcessor::new(Box::new(PassThrough), 0.0, 0.0, 0.0);
    let out = p.process_signal(&vec![0.1; 1000], &vec![0.0; 1200]).unwrap();
    assert_eq!(out.len(), 2 * FRAME);
}

#[test]
fn mic_delay_line() {
    let mic: Vec<f32> = (0..FRAME * 4).map(|i| i as f32 * 1e-4).collect();
    let mut p = AecProcessor::new(Box::new(PassThrough), 0.0, 10.0, 0.0);
    let out = p.process_signal(&mic, &vec![0.0; mic.len()]).unwrap();
    assert!(out[..FRAME].iter().all(|&v| v == 0.0));
    assert_eq!(&out[FRAME..], &mic[..FRAME * 3]);
}

#[test]
fn rejects_wrong_block_sizes() {
    for e in available_engines() {
        let mut eng = make_engine(e, &EngineOptions::default()).unwrap();
        let mut out = vec![0.0; 160];
        assert!(eng.process(&[0.0; 160], &[0.0; 160], &mut out).is_err(), "{e}");
        let mut out = vec![0.0; FRAME];
        assert!(eng.process(&[0.0; FRAME], &[0.0; 100], &mut out).is_err(), "{e}");
        let mut p = AecProcessor::new(eng, 0.0, 0.0, 0.0);
        let mut out = vec![0.0; 960];
        assert!(p.process(&[0.0; 960], &[0.0; 960], &mut out).is_err(), "{e}");
    }
}

#[test]
fn i16_conversion_matches_numpy() {
    assert_eq!(to_i16(0.0), 0);
    assert_eq!(to_i16(1.0), 32767);
    assert_eq!(to_i16(-1.0), -32767);
    assert_eq!(to_i16(2.0), 32767);
    assert_eq!(to_i16(-2.0), -32768);
    assert_eq!(to_i16(0.5 / 32767.0), 0); // half to even
    assert_eq!(from_i16(-32768), -1.0);
}

#[test]
fn nlms_cancels_white_noise_echo() {
    let e = erle_for("nlms", &EngineOptions::default());
    assert!(e > 8.0, "ERLE {e:.1} dB");
}

#[cfg(feature = "speex")]
#[test]
fn speex_cancels_white_noise_echo() {
    for ns in [false, true] {
        let opts = EngineOptions { noise_suppression: ns, ..Default::default() };
        let e = erle_for("speex", &opts);
        assert!(e > 8.0, "ns={ns}: ERLE {e:.1} dB");
    }
}

#[cfg(feature = "webrtc")]
#[test]
fn webrtc_cancels_white_noise_echo() {
    let e = erle_for("webrtc", &EngineOptions::default());
    assert!(e > 20.0, "ERLE {e:.1} dB");
}

/// Cost per 10 ms frame; run with `--nocapture` to see the numbers.
#[test]
fn bench_frame_cost() {
    let (mic, x) = white_echo(4.0, 240, 3);
    for e in available_engines() {
        let mut p = AecProcessor::new(make_engine(e, &EngineOptions::default()).unwrap(), 0.0, 0.0, 0.0);
        let mut out = vec![0.0; FRAME];
        let frames = mic.len() / FRAME;
        let t = Instant::now();
        for f in 0..frames {
            let s = f * FRAME;
            p.process(&mic[s..s + FRAME], &x[s..s + FRAME], &mut out).unwrap();
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / frames as f64;
        println!("[bench] {e:>6}: {us:8.1} us / 10 ms frame");
    }
}
