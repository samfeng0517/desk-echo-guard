//! "Hey Siri" detector for the Siri guard: a trigger on /s/-like onsets
//! (high-band energy bursts), gated by a small neural network that looks at
//! what came *before* the /s/ ("hey") and what follows it ("iri").
//!
//! The phrase cannot be recognised before it has been said, and waiting for
//! its end would cost ~1 s of delay.  So the decision is split in stages
//! around the /s/ onset (frame `f0`), each a tiny CNN over log-mel frames:
//!
//! * stage 0, at `f0`: do the last 0.4 s sound like "hey" followed by an
//!   /s/?  Only then a (short) notch region is opened, from the /s/ onset
//!   on.  The "hey" is already in the past, so this only needs the
//!   lookahead that covers the onset frame.  [`reaching_back`]
//!   (HeySiriDetector::reaching_back) starts the region 0.2 s earlier, over
//!   the "hey" itself, for a longer lookahead: wake-word models keep firing
//!   when only the second half of the phrase is altered.
//! * stage 1 and 2, 0.12 s and 0.25 s later: does it still sound like
//!   "hey si-" / "hey sir-"?  If so the region is extended, else it ends
//!   there ("hey Sam", 黑色 ... are released after 0.12-0.25 s).
//!
//! The weights are trained on synthetic speech (see `tools/heysiri`).

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

use crate::filters::Sos;
use crate::FS;

/// 10 ms analysis frame.
pub const HOP: usize = FS / 100;
/// Mel bands of the feature frames.
pub const NMEL: usize = 32;
/// Frames before the /s/ onset frame every stage looks at (0.4 s).
pub const PRE_FRAMES: usize = 40;
/// Frames after the onset frame seen by stage 0, 1 and 2.
pub const STAGE_POST: [usize; 3] = [0, 12, 25];

/// `scipy.signal.butter(4, [3500, 8000], btype="band", fs=48000,
/// output="sos")`, computed with scipy 1.18 (hardcoded so no filter design
/// code is needed at run time).
const SIBILANT_SOS: [f64; 24] = [
    0.003869518318520653,
    0.007739036637041306,
    0.003869518318520653,
    1.0,
    -1.013460567973713,
    0.5190352631425476,
    1.0,
    2.0,
    1.0,
    1.0,
    -1.3500867691742364,
    0.6196737138322412,
    1.0,
    -2.0,
    1.0,
    1.0,
    -0.9120336384649043,
    0.75382300422147,
    1.0,
    -2.0,
    1.0,
    1.0,
    -1.6593338172680228,
    0.861393550246356,
];

/// Per-frame /s/ decision (10 ms frames): the 3.5-8 kHz band holds most of
/// the frame's energy and is well above its running floor.
pub struct SibilantFrame {
    ratio: f64,
    rise: f64,
    min_level: f64,
    hb: Sos,
    floor: f64,
}

impl SibilantFrame {
    pub fn new(ratio: f64, rise_db: f64, min_level_db: f64) -> Self {
        Self {
            ratio,
            rise: 10f64.powf(rise_db / 10.0),
            min_level: 10f64.powf(min_level_db / 10.0),
            hb: Sos::new(&SIBILANT_SOS),
            floor: 1e-6,
        }
    }

    /// Feed one 10 ms frame; true when it is /s/-like.
    pub fn is_sibilant(&mut self, frame: &[f32]) -> bool {
        let n = frame.len() as f64;
        let (mut e_hb, mut e) = (0.0, 0.0);
        for &v in frame {
            let v = v as f64;
            let h = self.hb.process_sample(v);
            e_hb += h * h;
            e += v * v;
        }
        let e_hb = e_hb / n;
        let e = e / n + 1e-12;
        let is_s = e_hb > self.ratio * e && e_hb > self.rise * self.floor && e_hb > self.min_level;
        // noise-floor tracker: follows dips at once, creeps up slowly
        self.floor = if e_hb < self.floor {
            e_hb
        } else {
            self.floor * 1.002
        }
        .max(1e-10);
        is_s
    }
}

impl SibilantFrame {
    /// Thresholds of the detector: low enough to find quiet or noisy /s/
    /// sounds, the network sorts out the extra candidates.
    pub fn sensitive() -> Self {
        Self::new(0.35, 10.0, -65.0)
    }
}

/// Log-mel features: one frame per 10 ms, from a 20 ms Hann window (the
/// frame and the one before it), 32 triangular mel bands over 150-7500 Hz
/// (so 16 kHz material gives the same features), natural log of the power.
pub struct MelFrontend {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    prev: Vec<f32>,
    buf: Vec<f32>,
    spec: Vec<Complex<f32>>,
    /// per band: first bin, weights
    bands: Vec<(usize, Vec<f32>)>,
}

impl Default for MelFrontend {
    fn default() -> Self {
        Self::new()
    }
}

impl MelFrontend {
    const WIN: usize = 2 * HOP;

    pub fn new() -> Self {
        let n = Self::WIN;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let window = (0..n)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32)
            .collect();
        let mel = |f: f64| 2595.0 * (1.0 + f / 700.0).log10();
        let inv = |m: f64| 700.0 * (10f64.powf(m / 2595.0) - 1.0);
        let (lo, hi) = (mel(150.0), mel(7500.0));
        let edge = |k: usize| inv(lo + (hi - lo) * k as f64 / (NMEL + 1) as f64);
        let df = FS as f64 / n as f64;
        let bands = (0..NMEL)
            .map(|b| {
                let (f0, f1, f2) = (edge(b), edge(b + 1), edge(b + 2));
                let first = (f0 / df).ceil() as usize;
                let last = (f2 / df).floor() as usize;
                let w = (first..=last)
                    .map(|k| {
                        let f = k as f64 * df;
                        let v = if f <= f1 {
                            (f - f0) / (f1 - f0)
                        } else {
                            (f2 - f) / (f2 - f1)
                        };
                        v.max(0.0) as f32
                    })
                    .collect();
                (first, w)
            })
            .collect();
        Self {
            spec: fft.make_output_vec(),
            fft,
            window,
            prev: vec![0.0; HOP],
            buf: vec![0.0; n],
            bands,
        }
    }

    /// Feed one 10 ms frame, get its log-mel vector.
    pub fn frame(&mut self, x: &[f32], out: &mut [f32; NMEL]) {
        assert_eq!(x.len(), HOP);
        for (i, b) in self.buf.iter_mut().enumerate() {
            let v = if i < HOP { self.prev[i] } else { x[i - HOP] };
            *b = v * self.window[i];
        }
        self.prev.copy_from_slice(x);
        // the buffer length always matches the plan
        let _ = self.fft.process(&mut self.buf, &mut self.spec);
        for (o, (first, w)) in out.iter_mut().zip(&self.bands) {
            let e: f32 = w
                .iter()
                .zip(&self.spec[*first..])
                .map(|(w, c)| w * c.norm_sqr())
                .sum();
            *o = (e + 1e-10).ln();
        }
    }
}

/// Dynamic range (natural-log power) kept below the window's loudest band.
const RANGE: f32 = 11.5; // 50 dB

/// Turn a window of log-mel frames into the network input: level relative to
/// the loudest value, limited to 50 dB, scaled to `[-1, 1]`.  Gain invariant.
pub fn normalize(win: &mut [f32]) {
    let max = win.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    for v in win.iter_mut() {
        *v = ((*v - max).max(-RANGE) / RANGE) * 2.0 + 1.0;
    }
}

/// Weights of the three stages, written by `tools/heysiri/train.py`:
/// `"HSD1"`, then per stage `f32 threshold` and eight `u32 n, n × f32`
/// tensors (conv1 w/b, conv2 w/b, fc1 w/b, fc2 w/b; PyTorch layouts).
static WEIGHTS: &[u8] = include_bytes!("heysiri.bin");

const C1: usize = 24;
const C2: usize = 32;
const KERNEL: usize = 5;
const STRIDE: usize = 2;
const HIDDEN: usize = 32;

/// One stage: conv(32→24, k5, s2) → conv(24→32, k5, s2) → fc 32 → fc 1,
/// ReLU in between, over `frames` log-mel frames.
struct Stage {
    frames: usize,
    threshold: f32,
    c1w: Vec<f32>,
    c1b: Vec<f32>,
    c2w: Vec<f32>,
    c2b: Vec<f32>,
    f1w: Vec<f32>,
    f1b: Vec<f32>,
    f2w: Vec<f32>,
    f2b: Vec<f32>,
    // scratch
    input: Vec<f32>,
    a1: Vec<f32>,
    a2: Vec<f32>,
}

fn conv_len(n: usize) -> usize {
    (n - KERNEL) / STRIDE + 1
}

/// `y[o][t] = relu(b[o] + Σ_i Σ_k w[o][i][k] · x[i][t·STRIDE + k])`;
/// `x` is channel-major (`cin × tin`), `y` too.
fn conv_relu(x: &[f32], cin: usize, w: &[f32], b: &[f32], y: &mut [f32]) {
    let tin = x.len() / cin;
    let tout = conv_len(tin);
    for (o, (yo, &bo)) in y.chunks_exact_mut(tout).zip(b).enumerate() {
        yo.fill(bo);
        for i in 0..cin {
            let wk = &w[(o * cin + i) * KERNEL..][..KERNEL];
            let xi = &x[i * tin..][..tin];
            for (t, v) in yo.iter_mut().enumerate() {
                let xs = &xi[t * STRIDE..][..KERNEL];
                *v += wk.iter().zip(xs).map(|(a, b)| a * b).sum::<f32>();
            }
        }
        yo.iter_mut().for_each(|v| *v = v.max(0.0));
    }
}

impl Stage {
    fn parse(frames: usize, buf: &mut &[u8]) -> Self {
        fn f32s(buf: &mut &[u8], n: usize) -> Vec<f32> {
            let (head, rest) = buf.split_at(4 * n);
            *buf = rest;
            head.as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        }
        fn tensor(buf: &mut &[u8], n: usize) -> Vec<f32> {
            let (head, rest) = buf.split_at(4);
            *buf = rest;
            let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
            assert_eq!(len, n, "heysiri.bin: tensor size");
            f32s(buf, n)
        }
        let t1 = conv_len(frames);
        let t2 = conv_len(t1);
        let threshold = f32s(buf, 1)[0];
        Self {
            frames,
            threshold,
            c1w: tensor(buf, C1 * NMEL * KERNEL),
            c1b: tensor(buf, C1),
            c2w: tensor(buf, C2 * C1 * KERNEL),
            c2b: tensor(buf, C2),
            f1w: tensor(buf, HIDDEN * C2 * t2),
            f1b: tensor(buf, HIDDEN),
            f2w: tensor(buf, HIDDEN),
            f2b: tensor(buf, 1),
            input: vec![0.0; NMEL * frames],
            a1: vec![0.0; C1 * t1],
            a2: vec![0.0; C2 * t2],
        }
    }

    /// Logit for the window `frames` (oldest first); positive side of
    /// `threshold` means "sounds like hey siri so far".
    fn score<'a>(&mut self, frames: impl Iterator<Item = &'a [f32; NMEL]>) -> f32 {
        let n = self.frames;
        let mut count = 0;
        for (t, f) in frames.enumerate() {
            for (m, &v) in f.iter().enumerate() {
                self.input[m * n + t] = v;
            }
            count += 1;
        }
        debug_assert_eq!(count, n);
        normalize(&mut self.input);
        conv_relu(&self.input, NMEL, &self.c1w, &self.c1b, &mut self.a1);
        conv_relu(&self.a1, C1, &self.c2w, &self.c2b, &mut self.a2);
        let mut out = self.f2b[0];
        for ((w, &b), &w2) in self
            .f1w
            .chunks_exact(self.a2.len())
            .zip(&self.f1b)
            .zip(&self.f2w)
        {
            let h = b + w.iter().zip(&self.a2).map(|(a, b)| a * b).sum::<f32>();
            out += w2 * h.max(0.0);
        }
        out
    }
}

/// Counters of a [`HeySiriDetector`], for tests and logs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeySiriStats {
    /// /s/ onsets seen.
    pub onsets: u64,
    /// ... of which the audio before sounded like "hey" (notch opened).
    pub opened: u64,
    /// ... of which still sounded like "hey si" 0.12 s later.
    pub continued: u64,
    /// ... of which sounded like "hey sir" 0.25 s later (full length notch).
    pub confirmed: u64,
}

/// Detector for the guard's `heysiri` mode, see the module docs.
///
/// Regions: at the /s/ onset `t` (if stage 0 agrees) `[t - pre, t1)` with
/// `t1` the moment stage 1 decides; stage 1 and 2 each append a region that
/// starts where the previous one ends, up to `t + dur` in total.
pub struct HeySiriDetector {
    pre: i64,
    dur: i64,
    sib: SibilantFrame,
    mel: MelFrontend,
    stages: [Stage; 3],
    /// the last `PRE_FRAMES + 1 + STAGE_POST[2]` feature frames
    hist: std::collections::VecDeque<[f32; NMEL]>,
    prev: bool,
    /// open candidates: (onset frame number, next stage)
    open: Vec<(i64, usize)>,
    frame_no: i64,
    stats: HeySiriStats,
    pending: Vec<f32>,
    pending_start: i64,
}

impl Default for HeySiriDetector {
    fn default() -> Self {
        Self::new(0.03, 0.45)
    }
}

impl HeySiriDetector {
    const HIST: usize = PRE_FRAMES + 1 + STAGE_POST[2];

    /// `pre_s`: notch starts this long before the detected /s/ onset;
    /// `dur_s`: length of a fully confirmed notch from the onset.
    pub fn new(pre_s: f64, dur_s: f64) -> Self {
        let mut buf = WEIGHTS;
        assert_eq!(&buf[..4], b"HSD1", "heysiri.bin: magic");
        buf = &buf[4..];
        let stages = STAGE_POST.map(|post| Stage::parse(PRE_FRAMES + 1 + post, &mut buf));
        assert!(buf.is_empty(), "heysiri.bin: trailing data");
        let silence = [(1e-10f32).ln(); NMEL];
        Self {
            pre: (pre_s * FS as f64) as i64,
            dur: (dur_s * FS as f64) as i64,
            sib: SibilantFrame::sensitive(),
            mel: MelFrontend::new(),
            stages,
            hist: std::iter::repeat_n(silence, Self::HIST).collect(),
            prev: false,
            open: Vec::new(),
            frame_no: 0,
            stats: HeySiriStats::default(),
            pending: Vec::with_capacity(HOP),
            pending_start: 0,
        }
    }

    /// Like `default()`, but the region starts 0.2 s before the /s/ onset,
    /// over the "hey" (median length in the training clips), at the price
    /// of 0.17 s more lookahead.
    pub fn reaching_back() -> Self {
        Self::new(0.2, 0.45)
    }

    /// Samples marked before the onset.
    pub fn pre(&self) -> i64 {
        self.pre
    }

    pub fn stats(&self) -> HeySiriStats {
        self.stats
    }

    /// Score of `stage` for the window that ends with the newest frame.
    fn score(&mut self, stage: usize) -> bool {
        let st = &mut self.stages[stage];
        let skip = Self::HIST - st.frames;
        st.score(self.hist.iter().skip(skip)) >= st.threshold
    }

    /// End (sample index) of the region decided at `stage` for onset `t`.
    fn stage_end(&self, t: i64, stage: usize) -> i64 {
        match STAGE_POST.get(stage + 1) {
            // the next stage decides when its last frame is complete
            Some(&post) => (t + ((post + 1) * HOP) as i64).min(t + self.dur),
            None => t + self.dur,
        }
    }

    fn frame(&mut self, t: i64, out: &mut Vec<(i64, i64)>) {
        let is_s = self.sib.is_sibilant(&self.pending);
        let mut m = [0.0; NMEL];
        self.mel.frame(&self.pending, &mut m);
        self.hist.pop_front();
        self.hist.push_back(m);
        let no = self.frame_no;
        self.frame_no += 1;

        // later stages of the open candidates
        let mut i = 0;
        while i < self.open.len() {
            let (f0, stage) = self.open[i];
            if no - f0 < STAGE_POST[stage] as i64 {
                i += 1;
                continue;
            }
            let t0 = t - (no - f0) * HOP as i64;
            if self.score(stage) {
                out.push((self.stage_end(t0, stage - 1), self.stage_end(t0, stage)));
                if stage == 1 {
                    self.stats.continued += 1;
                    self.open[i].1 = 2;
                    i += 1;
                    continue;
                }
                self.stats.confirmed += 1;
            }
            self.open.swap_remove(i);
        }

        if is_s && !self.prev {
            self.stats.onsets += 1;
            if self.score(0) {
                self.stats.opened += 1;
                out.push((t - self.pre, self.stage_end(t, 0)));
                self.open.push((no, 1));
            }
        }
        self.prev = is_s;
    }
}

impl crate::Detector for HeySiriDetector {
    fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)> {
        if self.pending.is_empty() {
            self.pending_start = start;
        }
        let mut out = Vec::new();
        let mut rest = x;
        while !rest.is_empty() {
            let take = (HOP - self.pending.len()).min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == HOP {
                let t = self.pending_start;
                self.pending_start += HOP as i64;
                self.frame(t, &mut out);
                self.pending.clear();
            }
        }
        out
    }

    /// The first region starts at frame `t` minus `pre` and is reported once
    /// the frame's last sample `t + HOP - 1` has been fed; in the worst case
    /// that sample is the first one of the block, so the region start lies
    /// `HOP - 1 + pre` samples before the block start (independent of the
    /// block size).  The follow-up regions start in the future.
    fn report_delay(&self) -> usize {
        HOP - 1 + self.pre.max(0) as usize
    }
}
