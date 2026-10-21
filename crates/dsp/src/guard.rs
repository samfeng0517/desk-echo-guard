//! "Hey Siri" anti-trigger processing for the call output
//! (`siri_guard.SiriGuard` and the model-free detectors).
//!
//! The guard delays the stream by `lookahead` (~1 s), runs a detector on the
//! *undelayed* input, and when the phrase is detected ramps the mask `m(t)`
//! to 1 over the phrase region (still inside the delay buffer), then back to
//! 0: `out = dry - m(t) * (dry - wet)`, with `wet` = the notched signal.
//! `always` mode notches continuously (no detector, no lookahead).

use std::fmt;
use std::str::FromStr;

use anyhow::bail;

use crate::filters::Sos;
use crate::notch::{NotchBank, NotchConfig};
use crate::{DelayLine, Detector, FS};

/// Test helper: fixed regions known in advance.
pub struct OracleDetector {
    regions: Vec<(i64, i64)>,
    given: Vec<bool>,
}

impl OracleDetector {
    pub fn new(regions: Vec<(i64, i64)>) -> Self {
        let given = vec![false; regions.len()];
        Self { regions, given }
    }
}

impl Detector for OracleDetector {
    fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)> {
        let end = start + x.len() as i64;
        let mut out = Vec::new();
        for (i, &(a, b)) in self.regions.iter().enumerate() {
            // "detect" when the phrase has fully arrived (like a real KWS)
            if !self.given[i] && b <= end {
                self.given[i] = true;
                out.push((a, b));
            }
        }
        out
    }
}

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

/// Training-free trigger on /s/-like onsets (high-band energy burst).
///
/// 10 ms frames.  A frame is sibilant when the 3.5-8 kHz band holds most of
/// the energy and is well above its running floor.  On a sibilant onset at
/// t we notch `[t - pre_s, t + dur_s]` ("Siri" ~ 0.35-0.5 s from its /s/).
pub struct SibilantDetector {
    ratio: f64,
    rise: f64,
    min_level: f64,
    pre: i64,
    dur: i64,
    refractory: i64,
    hb: Sos,
    floor: f64,
    prev: bool,
    last: i64,
    pending: Vec<f32>,
    pending_start: i64,
}

impl Default for SibilantDetector {
    fn default() -> Self {
        Self::new(0.5, 15.0, -55.0, 0.03, 0.45, 0.25)
    }
}

impl SibilantDetector {
    pub fn new(
        ratio: f64,
        rise_db: f64,
        min_level_db: f64,
        pre_s: f64,
        dur_s: f64,
        refractory_s: f64,
    ) -> Self {
        let fs = FS as f64;
        Self {
            ratio,
            rise: 10f64.powf(rise_db / 10.0),
            min_level: 10f64.powf(min_level_db / 10.0),
            pre: (pre_s * fs) as i64,
            dur: (dur_s * fs) as i64,
            refractory: (refractory_s * fs) as i64,
            hb: Sos::new(&SIBILANT_SOS),
            floor: 1e-6,
            prev: false,
            last: -1_000_000_000_000,
            pending: Vec::with_capacity(FS / 100),
            pending_start: 0,
        }
    }

    /// Samples marked before the onset.
    pub fn pre(&self) -> i64 {
        self.pre
    }

    fn frame(&mut self, t: i64, out: &mut Vec<(i64, i64)>) {
        let n = self.pending.len() as f64;
        let (mut e_hb, mut e) = (0.0, 0.0);
        for &v in &self.pending {
            let v = v as f64;
            let h = self.hb.process_sample(v);
            e_hb += h * h;
            e += v * v;
        }
        let e_hb = e_hb / n;
        let e = e / n + 1e-12;
        let is_s = e_hb > self.ratio * e && e_hb > self.rise * self.floor && e_hb > self.min_level;
        if is_s && !self.prev && t - self.last > self.refractory {
            self.last = t;
            out.push((t - self.pre, t + self.dur));
        }
        self.prev = is_s;
        // noise-floor tracker: follows dips at once, creeps up slowly
        self.floor = if e_hb < self.floor {
            e_hb
        } else {
            self.floor * 1.002
        }
        .max(1e-10);
    }
}

impl Detector for SibilantDetector {
    fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)> {
        let f = FS / 100;
        if self.pending.is_empty() {
            self.pending_start = start;
        }
        let mut out = Vec::new();
        let mut rest = x;
        while !rest.is_empty() {
            let take = (f - self.pending.len()).min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == f {
                let t = self.pending_start;
                self.pending_start += f as i64;
                self.frame(t, &mut out);
                self.pending.clear();
            }
        }
        out
    }
}

// --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardMode {
    Always,
    Sibilant,
    Detect,
    Off,
}

impl GuardMode {
    pub const ALL: [GuardMode; 4] = [
        GuardMode::Detect,
        GuardMode::Sibilant,
        GuardMode::Always,
        GuardMode::Off,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            GuardMode::Always => "always",
            GuardMode::Sibilant => "sibilant",
            GuardMode::Detect => "detect",
            GuardMode::Off => "off",
        }
    }

    /// Modes that run a detector and need the lookahead.
    pub fn uses_detector(self) -> bool {
        matches!(self, GuardMode::Detect | GuardMode::Sibilant)
    }
}

impl FromStr for GuardMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "always" => GuardMode::Always,
            "sibilant" => GuardMode::Sibilant,
            "detect" => GuardMode::Detect,
            "off" => GuardMode::Off,
            _ => bail!("unknown siri guard mode {s:?}; choose from detect, sibilant, always, off"),
        })
    }
}

impl fmt::Display for GuardMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GuardConfig {
    pub mode: GuardMode,
    pub lookahead_s: f64,
    pub ramp_ms: f64,
    pub notch: NotchConfig,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            mode: GuardMode::Detect,
            lookahead_s: 1.0,
            ramp_ms: 15.0,
            notch: NotchConfig::default(),
        }
    }
}

/// Streaming processor: `process(block, out)` (same length).
///
/// Latency: notch group delay (12.5 ms default) + lookahead (0 in `always`
/// mode; `off` passes the input through untouched, latency 0).
pub struct SiriGuard {
    cfg: GuardConfig,
    notch: NotchBank,
    lookahead: usize,
    dry_align: DelayLine,
    dry_look: DelayLine,
    wet_look: DelayLine,
    detector: Option<Box<dyn Detector>>,
    ramp: i64,
    latency: usize,
    regions: Vec<(i64, i64)>,
    detections: u64,
    /// absolute index of the next input sample
    t: i64,
    wet: Vec<f32>,
    dry: Vec<f32>,
    tmp: Vec<f32>,
    mask: Vec<f32>,
}

impl SiriGuard {
    pub fn new(cfg: GuardConfig, detector: Option<Box<dyn Detector>>) -> anyhow::Result<Self> {
        if cfg.mode.uses_detector() && detector.is_none() {
            bail!("mode {:?} needs a detector", cfg.mode.as_str());
        }
        let notch = NotchBank::new(cfg.notch.clone());
        let lookahead = if cfg.mode.uses_detector() {
            (cfg.lookahead_s * FS as f64) as usize
        } else {
            0
        };
        let latency = if cfg.mode == GuardMode::Off {
            0
        } else {
            notch.delay() + lookahead
        };
        Ok(Self {
            ramp: ((cfg.ramp_ms * FS as f64 / 1000.0) as i64).max(1),
            dry_align: DelayLine::new(notch.delay()),
            dry_look: DelayLine::new(lookahead),
            wet_look: DelayLine::new(lookahead),
            notch,
            lookahead,
            detector,
            latency,
            regions: Vec::new(),
            detections: 0,
            t: 0,
            wet: Vec::new(),
            dry: Vec::new(),
            tmp: Vec::new(),
            mask: Vec::new(),
            cfg,
        })
    }

    pub fn config(&self) -> &GuardConfig {
        &self.cfg
    }

    pub fn notch_config(&self) -> &NotchConfig {
        self.notch.config()
    }

    /// Total delay in samples.
    pub fn latency(&self) -> usize {
        self.latency
    }

    pub fn lookahead(&self) -> usize {
        self.lookahead
    }

    /// Cross-fade ramp length in samples.
    pub fn ramp(&self) -> usize {
        self.ramp as usize
    }

    pub fn detections(&self) -> u64 {
        self.detections
    }

    /// Fill `self.mask` for *input* indices `[s, s+n)` and prune regions that
    /// are fully in the past.
    fn fill_mask(&mut self, s: i64, n: usize) {
        self.mask.clear();
        if self.cfg.mode == GuardMode::Always {
            self.mask.resize(n, 1.0);
            return;
        }
        self.mask.resize(n, 0.0);
        let r = self.ramp;
        let e = s + n as i64;
        let mask = &mut self.mask;
        self.regions.retain(|&(_, b)| b + r >= s);
        for &(a, b) in &self.regions {
            if a - r >= e {
                continue;
            }
            let mut put = |lo: i64, hi: i64, f: &dyn Fn(i64) -> f32| {
                for idx in lo.max(s)..hi.min(e) {
                    let m = &mut mask[(idx - s) as usize];
                    let v = f(idx);
                    if v > *m {
                        *m = v;
                    }
                }
            };
            let pi = std::f64::consts::PI;
            put(a - r, a, &|idx| {
                (0.5 - 0.5 * (pi * (idx - (a - r)) as f64 / r as f64).cos()) as f32
            });
            put(a, b, &|_| 1.0);
            put(b, b + r, &|idx| {
                (0.5 + 0.5 * (pi * (idx - b) as f64 / r as f64).cos()) as f32
            });
        }
    }

    pub fn process(&mut self, x: &[f32], out: &mut [f32]) {
        assert_eq!(x.len(), out.len());
        let n = x.len();
        if self.cfg.mode == GuardMode::Off {
            out.copy_from_slice(x);
            return;
        }
        if self.cfg.mode.uses_detector() {
            if let Some(det) = self.detector.as_mut() {
                for (a, b) in det.feed(x, self.t) {
                    // earliest input index still ahead of (or in) this output block
                    let a = a.max(self.t - self.latency as i64);
                    if b > a {
                        self.regions.push((a, b));
                        self.detections += 1;
                    }
                }
            }
        }
        self.wet.resize(n, 0.0);
        self.dry.resize(n, 0.0);
        self.tmp.resize(n, 0.0);
        self.notch.process(x, &mut self.tmp);
        self.wet_look.process(&self.tmp, &mut self.wet);
        self.dry_align.process(x, &mut self.tmp);
        self.dry_look.process(&self.tmp, &mut self.dry);
        // output sample k of this block corresponds to input index t + k - latency
        self.fill_mask(self.t - self.latency as i64, n);
        self.t += n as i64;
        for (((o, &d), &w), &m) in out.iter_mut().zip(&self.dry).zip(&self.wet).zip(&self.mask) {
            *o = d - m * (d - w);
        }
    }

    /// Offline helper; output is re-aligned to the input (latency removed).
    pub fn process_signal(&mut self, x: &[f32], block: usize) -> Vec<f32> {
        let block = block.max(1);
        let total = (x.len() + self.latency).div_ceil(block) * block;
        let mut src = vec![0.0f32; total];
        src[..x.len()].copy_from_slice(x);
        let mut out = vec![0.0f32; total];
        for (s, o) in src.chunks(block).zip(out.chunks_mut(block)) {
            self.process(s, o);
        }
        out[self.latency..self.latency + x.len()].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{
        align, band_energy_db, db, energy, envelope_correlation, log_spectral_distance,
    };
    use crate::testsig::{synth_speech, synth_speech_with};
    use rand::{rngs::StdRng, SeedableRng};
    use rand_distr::{Distribution, StandardNormal};

    const FSI: i64 = FS as i64;

    fn guard_always(profile: &str) -> SiriGuard {
        let cfg = GuardConfig {
            mode: GuardMode::Always,
            notch: NotchConfig::profile(profile, None).unwrap(),
            ..Default::default()
        };
        SiriGuard::new(cfg, None).unwrap()
    }

    #[test]
    fn always_mode_spectral_effect_and_intelligibility() {
        for profile in ["deep-wide", "apple-deep", "formant"] {
            let x = synth_speech(8.0, 5);
            let mut g = guard_always(profile);
            assert_eq!(g.latency(), 600);
            let y = g.process_signal(&x, 480);
            let bands = g.notch_config().bands.clone();
            let drop = bands
                .iter()
                .map(|&(lo, hi)| band_energy_db(&x, lo, hi) - band_energy_db(&y, lo, hi))
                .fold(f64::INFINITY, f64::min);
            let total = db(energy(&y)) - db(energy(&x));
            let lsd_all = log_spectral_distance(&x, &y, 0.0, None);
            let lowest = bands.iter().map(|b| b.0).fold(f64::INFINITY, f64::min);
            let lsd_outside = log_spectral_distance(&x, &y, 100.0, Some(lowest - 300.0));
            let (xa, ya) = align(&x, &y, 2400);
            let env = envelope_correlation(xa, ya);
            println!(
                "[metrics] siri_guard_always[{profile}]: band_attenuation_dB_min={drop:.2}, \
                 total_energy_change_dB={total:.2}, LSD_dB={lsd_all:.2}, \
                 LSD_outside_bands_dB={lsd_outside:.2}, envelope_corr={env:.2}"
            );
            assert!(drop > 20.0, "{profile}: {drop}"); // the targeted bands are really gone
            assert!(lsd_outside < 0.5, "{profile}: {lsd_outside}"); // rest untouched
            assert!(total > -3.0, "{profile}: {total}"); // most speech energy is below 3 kHz
            assert!(env > 0.9, "{profile}: {env}");
        }
    }

    #[test]
    fn detect_mode_only_touches_phrase_and_uses_lookahead() {
        let x = synth_speech(6.0, 6);
        let (a, b) = (2 * FSI, (2.7 * FS as f64) as i64); // the "Hey Siri" region
        let det = OracleDetector::new(vec![(a, b)]); // fires once the phrase has ended
        let cfg = GuardConfig {
            mode: GuardMode::Detect,
            lookahead_s: 1.0,
            ..Default::default()
        };
        let mut g = SiriGuard::new(cfg, Some(Box::new(det))).unwrap();
        let y = g.process_signal(&x, 480);
        let r = g.ramp() as i64;
        let (a_u, b_u) = (a as usize, b as usize);
        // FIR half-length margin
        let outside = (0..(a - r - 700) as usize).chain((b + r + 700) as usize..x.len());
        let max_out = outside.map(|i| (y[i] - x[i]).abs()).fold(0.0f32, f32::max);
        assert!(
            max_out < 1e-4,
            "audio outside the phrase must be bit-identical-ish: {max_out}"
        );
        let (sx, sy) = (&x[a_u..b_u], &y[a_u..b_u]);
        let drop = g
            .notch_config()
            .bands
            .iter()
            .map(|&(lo, hi)| band_energy_db(sx, lo, hi) - band_energy_db(sy, lo, hi))
            .fold(f64::INFINITY, f64::min);
        println!(
            "[metrics] siri_guard_detect[oracle]: phrase_band_attenuation_dB_min={drop:.2}, \
             latency_ms={:.2}, outside_max_abs_diff={max_out:.2e}",
            g.latency() as f64 / FS as f64 * 1000.0
        );
        assert!(drop > 20.0);
        assert_eq!(g.detections(), 1);
    }

    #[test]
    fn detection_too_late_is_clamped_not_crashing() {
        let x = synth_speech(3.0, 7);
        let det = OracleDetector::new(vec![((0.2 * FS as f64) as i64, FSI)]);
        let cfg = GuardConfig {
            mode: GuardMode::Detect,
            lookahead_s: 0.2, // lookahead too short
            ..Default::default()
        };
        let mut g = SiriGuard::new(cfg, Some(Box::new(det))).unwrap();
        let y = g.process_signal(&x, 480);
        assert_eq!(y.len(), x.len());
        assert!(y.iter().all(|v| v.is_finite()));
        assert_eq!(g.detections(), 1);
    }

    /// `butter(4, [4000, 8000], btype="band", fs=48000, output="sos")` (scipy).
    const HISS_SOS: [f64; 24] = [
        0.002576434425322621,
        0.005152868850645242,
        0.002576434425322621,
        1.0,
        -1.0186463504443495,
        0.5709605475805424,
        1.0,
        2.0,
        1.0,
        1.0,
        -1.3127613067738024,
        0.6446074435526299,
        1.0,
        -2.0,
        1.0,
        1.0,
        -0.9246211099723749,
        0.7843620196495954,
        1.0,
        -2.0,
        1.0,
        1.0,
        -1.6037840338742153,
        0.865390521855695,
    ];
    /// `butter(10, 2500, fs=48000, output="sos")` (scipy).
    const LP_SOS: [f64; 30] = [
        5.254783747600244e-09,
        1.0509567495200489e-08,
        5.254783747600244e-09,
        1.0,
        -1.4374847182847552,
        0.5180472914630023,
        1.0,
        2.0,
        1.0,
        1.0,
        -1.47221190720424,
        0.5547207352977664,
        1.0,
        2.0,
        1.0,
        1.0,
        -1.5431211312373858,
        0.629604004743374,
        1.0,
        2.0,
        1.0,
        1.0,
        -1.6526834038637974,
        0.7453065990676554,
        1.0,
        2.0,
        1.0,
        1.0,
        -1.8031883556091324,
        0.9042464691355587,
    ];

    #[test]
    fn sibilant_detector_fires_on_s_like_bursts() {
        let mut rng = StdRng::seed_from_u64(0);
        let mut x = vec![0.0f32; 3 * FS];
        let noise: Vec<f64> = (0..(0.12 * FS as f64) as usize)
            .map(|_| StandardNormal.sample(&mut rng))
            .collect();
        let hiss = Sos::new(&HISS_SOS).process(&noise);
        let sd = crate::filters::std(&hiss);
        let onsets = [
            FS / 2,
            (1.5 * FS as f64) as usize,
            (2.4 * FS as f64) as usize,
        ];
        for &o in &onsets {
            for (k, &h) in hiss.iter().enumerate() {
                x[o + k] += (0.1 * h / sd) as f32;
            }
        }
        let mut det = SibilantDetector::default();
        let mut regs = Vec::new();
        for (k, c) in x.chunks(480).enumerate() {
            regs.extend(det.feed(c, (k * 480) as i64));
        }
        let mut starts: Vec<i64> = regs.iter().map(|r| r.0).collect();
        starts.sort_unstable();
        assert_eq!(starts.len(), 3, "{regs:?}");
        for (&o, &s) in onsets.iter().zip(&starts) {
            assert!(((s + det.pre()) - o as i64).abs() < (0.02 * FS as f64) as i64);
        }
        // speech with its high band removed (no /s/ energy) must not trigger
        let vowel = synth_speech_with(3.0, 120.0, 8, 0.0);
        let v64: Vec<f64> = vowel.iter().map(|&v| v as f64).collect();
        let voiced: Vec<f32> = Sos::new(&LP_SOS)
            .process(&v64)
            .iter()
            .map(|&v| v as f32)
            .collect();
        let mut det2 = SibilantDetector::default();
        let n2: usize = voiced
            .chunks(480)
            .enumerate()
            .map(|(k, c)| det2.feed(c, (k * 480) as i64).len())
            .sum();
        assert_eq!(n2, 0);
        // odd block sizes give the same regions
        let mut det3 = SibilantDetector::default();
        let mut regs3 = Vec::new();
        let mut i = 0;
        for k in 0.. {
            if i >= x.len() {
                break;
            }
            let n = [1usize, 333, 1000, 7][k % 4].min(x.len() - i);
            regs3.extend(det3.feed(&x[i..i + n], i as i64));
            i += n;
        }
        assert_eq!(regs, regs3);
    }

    #[test]
    fn modes_and_errors() {
        for m in GuardMode::ALL {
            assert_eq!(m.as_str().parse::<GuardMode>().unwrap(), m);
        }
        assert!("loud".parse::<GuardMode>().is_err());
        let cfg = GuardConfig {
            mode: GuardMode::Sibilant,
            ..Default::default()
        };
        assert!(SiriGuard::new(cfg.clone(), None).is_err());
        let g = SiriGuard::new(cfg, Some(Box::new(SibilantDetector::default()))).unwrap();
        assert_eq!(g.latency(), 600 + FS);
        // off: untouched passthrough
        let mut off = SiriGuard::new(
            GuardConfig {
                mode: GuardMode::Off,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let x = synth_speech(0.5, 1);
        assert_eq!(off.process_signal(&x, 480), x);
    }
}
