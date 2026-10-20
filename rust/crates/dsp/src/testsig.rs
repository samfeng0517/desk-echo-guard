//! Synthetic test signals (`signals.py`, no recordings needed): speech-like
//! source, room impulse response, and an echo scenario
//! `mic = near + h * ref + noise`.  Also used by the run-time self-test.
//!
//! The random streams differ from numpy's, the statistics do not.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, StandardNormal};

use crate::filters::{fft_convolve, lfilter, lfilter_into, std};
use crate::FS;

/// (F1, F2, F3) for a handful of vowels, Hz.
const VOWELS: [[f64; 3]; 7] = [
    [730.0, 1090.0, 2440.0],
    [270.0, 2290.0, 3010.0],
    [300.0, 870.0, 2240.0],
    [530.0, 1840.0, 2480.0],
    [570.0, 840.0, 2410.0],
    [440.0, 1020.0, 2240.0],
    [660.0, 1720.0, 2410.0],
];

/// `scipy.signal.butter(4, [3000, 8000], btype="band", fs=48000)` (b, a),
/// computed with scipy 1.18.
const FRIC_B: [f64; 9] = [
    0.005541768332032851,
    0.0,
    -0.022167073328131403,
    0.0,
    0.0332506099921971,
    0.0,
    -0.022167073328131403,
    0.0,
    0.005541768332032851,
];
const FRIC_A: [f64; 9] = [
    1.0,
    -5.003992737440357,
    11.831810889422407,
    -17.159527159774996,
    16.664112478333713,
    -11.073715473889035,
    4.920229308319373,
    -1.3411160569080858,
    0.17423010478293122,
];
/// `scipy.signal.butter(2, [80, 12000], btype="band", fs=48000)` (b, a):
/// speaker / mic response of the room IR.
const IR_B: [f64; 5] = [
    0.28983268681878394,
    0.0,
    -0.5796653736375679,
    0.0,
    0.28983268681878394,
];
const IR_A: [f64; 5] = [
    1.0,
    -1.9913058776370296,
    1.1715994841437947,
    -0.3517646115643973,
    0.17159948414379478,
];

fn randn(rng: &mut StdRng, n: usize) -> Vec<f64> {
    (0..n).map(|_| StandardNormal.sample(rng)).collect()
}

/// `synth_speech(dur_s, seed=seed)` with the Python defaults
/// (f0 = 120 Hz, pause_prob = 0.15).
pub fn synth_speech(dur_s: f64, seed: u64) -> Vec<f32> {
    synth_speech_with(dur_s, 120.0, seed, 0.15)
}

/// Speech-like signal: jittered glottal pulse train through time-varying
/// formant resonators, syllabic envelope (~4 Hz), plus fricative noise bursts
/// (energy 3-8 kHz, like /s/) and pauses.  Normalised to RMS 0.05 (peaks
/// ~ -6 dBFS) over active parts.
pub fn synth_speech_with(dur_s: f64, f0: f64, seed: u64, pause_prob: f64) -> Vec<f32> {
    let fs = FS as f64;
    let mut rng = StdRng::seed_from_u64(seed);
    let n = (dur_s * fs) as usize;
    let mut out = vec![0.0f64; n];
    let mut pos = 0;
    while pos < n {
        let syl = (rng.gen_range(0.15..0.32) * fs) as usize;
        let seg_end = n.min(pos + syl);
        let l = seg_end - pos;
        if rng.gen::<f64>() < pause_prob {
            pos = seg_end;
            continue;
        }
        let seg: Vec<f64> = if rng.gen::<f64>() < 0.3 {
            // fricative: band-passed noise, 3-8 kHz, ~10 dB below vowels
            let mut seg = lfilter(&FRIC_B, &FRIC_A, &randn(&mut rng, l));
            let g = 0.3 / std(&seg).max(1e-12);
            seg.iter_mut().for_each(|v| *v *= g);
            seg
        } else {
            voiced_segment(&mut rng, l, f0)
        };
        for (k, v) in seg.iter().enumerate() {
            let env = (std::f64::consts::PI * k as f64 / l as f64).sin().powf(0.6);
            out[pos + k] = v * env;
        }
        pos = seg_end;
    }
    let (mut acc, mut cnt) = (0.0, 0usize);
    for v in &out {
        if v.abs() > 1e-6 {
            acc += v * v;
            cnt += 1;
        }
    }
    let rms = if cnt > 0 {
        (acc / cnt as f64).sqrt()
    } else {
        1.0
    };
    out.iter().map(|v| (v * 0.05 / rms) as f32).collect()
}

fn voiced_segment(rng: &mut StdRng, l: usize, f0: f64) -> Vec<f64> {
    let fs = FS as f64;
    // intonation contour (+-30 %), per-period jitter, shimmer
    let rate = rng.gen_range(0.5..2.0);
    // 6.28 as in signals.py (not exactly 2 pi)
    #[allow(clippy::approx_constant)]
    let ph0 = rng.gen_range(0.0..6.28);
    let mut phase = 0.0;
    let mut k_prev = 0.0;
    let mut pulses = vec![0.0f64; l];
    for (i, p) in pulses.iter_mut().enumerate() {
        let f = f0 * (1.0 + 0.3 * (2.0 * std::f64::consts::PI * rate * i as f64 / fs + ph0).sin());
        phase += f / fs;
        let k = phase.floor();
        // np.diff(k, prepend=k[0])
        *p = if i == 0 { 0.0 } else { k - k_prev };
        k_prev = k;
    }
    for p in pulses.iter_mut() {
        *p *= 1.0 + 0.2 * Distribution::<f64>::sample(&StandardNormal, rng);
    }
    let jmax = (0.0005 * fs) as i64;
    let mut moved = vec![0.0f64; l];
    for (i, &p) in pulses.iter().enumerate() {
        let jit = rng.gen_range(-jmax..=jmax);
        let j = if p != 0.0 { jit } else { 0 };
        let t = (i as i64 + j).clamp(0, l as i64 - 1) as usize;
        moved[t] += p;
    }
    let mut src = lfilter(&[1.0], &[1.0, -0.95], &moved); // glottal tilt
    let breath = lfilter(&[1.0], &[1.0, -0.7], &randn(rng, l));
    let (ss, sb) = (std(&src), std(&breath));
    if sb > 0.0 {
        let g = 0.25 * ss / sb; // aspiration
        src.iter_mut().zip(&breath).for_each(|(s, b)| *s += g * b);
    }
    // formants glide between two vowels
    let v0 = VOWELS[rng.gen_range(0..VOWELS.len())];
    let v1 = VOWELS[rng.gen_range(0..VOWELS.len())];
    let mut seg = vec![0.0f64; l];
    let nsub = 8;
    let edges: Vec<usize> = (0..=nsub)
        .map(|j| (j as f64 * (l as f64 / nsub as f64)) as usize)
        .collect();
    let mut zis = [[0.0f64; 2]; 3];
    let mut part = Vec::with_capacity(l / nsub + 1);
    let mut tmp = Vec::with_capacity(l / nsub + 1);
    for j in 0..nsub {
        part.clear();
        part.extend_from_slice(&src[edges[j]..edges[j + 1]]);
        for (q, bw) in [80.0, 100.0, 140.0].iter().enumerate() {
            let fc = v0[q] + (v1[q] - v0[q]) * (j as f64 + 0.5) / nsub as f64;
            let r = (-std::f64::consts::PI * bw / fs).exp();
            let th = 2.0 * std::f64::consts::PI * fc / fs;
            tmp.resize(part.len(), 0.0);
            lfilter_into(
                &[1.0 - r],
                &[1.0, -2.0 * r * th.cos(), r * r],
                &part,
                &mut zis[q],
                &mut tmp,
            );
            std::mem::swap(&mut part, &mut tmp);
        }
        seg[edges[j]..edges[j + 1]].copy_from_slice(&part);
    }
    let sd = std(&seg) + 1e-12;
    seg.iter_mut().for_each(|v| *v /= sd);
    seg
}

/// Room impulse response options (`signals.room_ir` keyword arguments).
#[derive(Clone, Copy, Debug)]
pub struct RoomIr {
    pub length_ms: f64,
    pub rt60_ms: f64,
    pub direct_delay_ms: f64,
    /// Peak amplitude of the IR (acoustic coupling).
    pub gain: f64,
    /// Direct-to-reverberant energy ratio.
    pub drr_db: f64,
    pub seed: u64,
}

impl Default for RoomIr {
    fn default() -> Self {
        Self {
            length_ms: 150.0,
            rt60_ms: 200.0,
            direct_delay_ms: 3.0,
            gain: 0.5,
            drr_db: 0.0,
            seed: 1,
        }
    }
}

/// Direct path (speaker -> mic) + exponentially decaying noise tail
/// (reflections), through a 80 Hz - 12 kHz speaker / mic response.
pub fn room_ir(o: &RoomIr) -> Vec<f32> {
    let fs = FS as f64;
    let mut rng = StdRng::seed_from_u64(o.seed);
    let n = (fs * o.length_ms / 1000.0) as usize;
    let d = (fs * o.direct_delay_ms / 1000.0) as usize;
    let nt = n.saturating_sub(d + 1);
    let mut tail = randn(&mut rng, nt);
    for (i, v) in tail.iter_mut().enumerate() {
        *v *= (-6.9 * (i as f64 / fs) / (o.rt60_ms / 1000.0)).exp();
    }
    let e: f64 = tail.iter().map(|v| v * v).sum();
    if e > 0.0 {
        let g = (10f64.powf(-o.drr_db / 10.0) / e).sqrt();
        tail.iter_mut().for_each(|v| *v *= g);
    }
    let mut h = vec![0.0f64; n];
    h[d] = 1.0;
    h[d + 1..].copy_from_slice(&tail);
    let h = lfilter(&IR_B, &IR_A, &h);
    let peak = h.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    h.iter().map(|v| (o.gain * v / peak) as f32).collect()
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub mic: Vec<f32>,
    pub reference: Vec<f32>,
    pub near: Vec<f32>,
    pub echo: Vec<f32>,
    /// Per-sample masks.
    pub far_only: Vec<bool>,
    pub near_only: Vec<bool>,
    pub double_talk: Vec<bool>,
}

/// `make_scenario` keyword arguments.
#[derive(Clone, Debug)]
pub struct ScenarioOptions {
    pub dur_s: f64,
    /// Extra (bulk) delay between the reference and its echo in the mic, on
    /// top of the IR's direct path (3 ms).
    pub delay_ms: f64,
    pub echo_gain: f64,
    pub noise_db: f64,
    pub nonlinear: f64,
    pub seed: u64,
    /// Far / near speech (e.g. real recordings); tiled / truncated to dur_s.
    pub far: Option<Vec<f32>>,
    pub near: Option<Vec<f32>>,
}

impl Default for ScenarioOptions {
    fn default() -> Self {
        Self {
            dur_s: 24.0,
            delay_ms: 40.0,
            echo_gain: 0.5,
            noise_db: -60.0,
            nonlinear: 0.0,
            seed: 0,
            far: None,
            near: None,
        }
    }
}

/// `np.resize`: tile / truncate to `n`.
fn resize_tiled(x: &[f32], n: usize) -> Vec<f32> {
    if x.is_empty() {
        return vec![0.0; n];
    }
    x.iter().copied().cycle().take(n).collect()
}

/// Layout: [0, 40%) far end only, [40%, 60%) near end only, [60%, 100%)
/// double talk.
pub fn make_scenario(dur_s: f64, delay_ms: f64, seed: u64) -> Scenario {
    make_scenario_with(&ScenarioOptions {
        dur_s,
        delay_ms,
        seed,
        ..Default::default()
    })
}

pub fn make_scenario_with(o: &ScenarioOptions) -> Scenario {
    let fs = FS as f64;
    let n = (o.dur_s * fs) as usize;
    let far = match &o.far {
        Some(f) => resize_tiled(f, n),
        None => resize_tiled(&synth_speech_with(o.dur_s, 210.0, o.seed + 10, 0.1), n),
    };
    let near_full = match &o.near {
        Some(f) => resize_tiled(f, n),
        None => resize_tiled(&synth_speech_with(o.dur_s, 110.0, o.seed + 20, 0.15), n),
    };
    let (a, b) = ((0.4 * n as f64) as usize, (0.6 * n as f64) as usize);
    let mut reference = far;
    reference[a..b].fill(0.0);
    let mut near = near_full;
    near[..a].fill(0.0);
    let h: Vec<f64> = room_ir(&RoomIr {
        gain: o.echo_gain,
        seed: o.seed + 1,
        ..Default::default()
    })
    .iter()
    .map(|&v| v as f64)
    .collect();
    let k = 1.0 + 10.0 * o.nonlinear;
    let drive: Vec<f64> = reference
        .iter()
        .map(|&v| {
            let v = v as f64;
            if o.nonlinear <= 0.0 {
                v
            } else {
                (v * k).tanh() / k
            }
        })
        .collect();
    let conv = fft_convolve(&drive, &h);
    let dly = (fs * o.delay_ms / 1000.0) as usize;
    let mut echo = vec![0.0f32; n];
    for i in dly..n {
        echo[i] = conv[i - dly] as f32;
    }
    let mut rng = StdRng::seed_from_u64(o.seed + 30);
    let ng = 10f64.powf(o.noise_db / 20.0);
    let mic: Vec<f32> = (0..n)
        .map(|i| {
            let z: f64 = StandardNormal.sample(&mut rng);
            near[i] + echo[i] + (z * ng) as f32
        })
        .collect();
    Scenario {
        mic,
        reference,
        near,
        echo,
        far_only: (0..n).map(|i| i < a).collect(),
        near_only: (0..n).map(|i| i >= a && i < b).collect(),
        double_talk: (0..n).map(|i| i >= b).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{band_energy_db, db, energy};

    #[test]
    fn synth_speech_statistics() {
        let x = synth_speech(10.0, 0);
        assert_eq!(x.len(), 480_000);
        let active: Vec<f64> = x
            .iter()
            .filter(|v| v.abs() > 1e-6)
            .map(|&v| v as f64)
            .collect();
        let rms = (active.iter().map(|v| v * v).sum::<f64>() / active.len() as f64).sqrt();
        assert!((rms - 0.05).abs() < 1e-4, "{rms}");
        let frac = active.len() as f64 / x.len() as f64;
        // (Python, seeds 0-3: 0.80 .. 0.89)
        assert!(frac > 0.7 && frac < 0.97, "active fraction {frac}");
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak < 0.7 && peak > 0.2, "{peak}");
        // speech-like spectrum: most energy below 3 kHz, some /s/ energy above
        let lo = band_energy_db(&x, 100.0, 3000.0);
        let hi = band_energy_db(&x, 3000.0, 8000.0);
        // (Python, seeds 0-3: 13.5 .. 15.8 dB)
        assert!(lo - hi > 10.0 && lo - hi < 20.0, "lo {lo} hi {hi}");
        println!(
            "synth_speech: active {frac:.3} peak {peak:.3} lo-hi {:.2}",
            lo - hi
        );
        // deterministic for a seed, different across seeds
        assert_eq!(x, synth_speech(10.0, 0));
        assert_ne!(x, synth_speech(10.0, 1));
        // no pauses -> (almost) everything active
        let y = synth_speech_with(3.0, 120.0, 8, 0.0);
        let fy = y.iter().filter(|v| v.abs() > 1e-6).count() as f64 / y.len() as f64;
        assert!(fy > 0.9, "{fy}");
    }

    #[test]
    fn room_ir_shape() {
        let h = room_ir(&RoomIr::default());
        assert_eq!(h.len(), 7200);
        let (k, peak) = h.iter().enumerate().fold((0, 0.0f32), |(bk, bv), (i, &v)| {
            if v.abs() > bv {
                (i, v.abs())
            } else {
                (bk, bv)
            }
        });
        assert!((peak - 0.5).abs() < 1e-6);
        assert!((143..=150).contains(&k), "direct path at {k}");
        assert!(h[..140].iter().all(|v| v.abs() < 1e-3));
    }

    #[test]
    fn scenario_layout_and_masks() {
        let sc = make_scenario(5.0, 40.0, 0);
        let n = 5 * FS;
        for v in [&sc.mic, &sc.reference, &sc.near, &sc.echo] {
            assert_eq!(v.len(), n);
        }
        let (a, b) = (2 * FS, 3 * FS);
        assert!(sc.far_only[a - 1] && !sc.far_only[a]);
        assert!(sc.near_only[a] && !sc.near_only[b]);
        assert!(sc.double_talk[b] && !sc.double_talk[b - 1]);
        assert!(sc.reference[a..b].iter().all(|&v| v == 0.0));
        assert!(sc.near[..a].iter().all(|&v| v == 0.0));
        // echo starts after the bulk delay + direct path
        let d = 40 * 48 + 140;
        assert!(sc.echo[..d].iter().all(|v| v.abs() < 1e-3));
        // echo level: about as loud as the far end (Python: -0.5 .. -1.7 dB)
        let erl = db(energy(&sc.reference[..a])) - db(energy(&sc.echo[..a]));
        assert!(erl > -4.0 && erl < 2.0, "{erl}");
        // noise floor ~ -60 dB in the near-only part is dominated by speech
        let nm: Vec<f32> = (a..b)
            .map(|i| sc.mic[i] - sc.near[i] - sc.echo[i])
            .collect();
        let nd = db(energy(&nm));
        assert!((nd + 60.0).abs() < 1.0, "{nd}");
    }
}
