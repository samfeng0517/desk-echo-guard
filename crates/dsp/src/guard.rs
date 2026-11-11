//! "Hey Siri" anti-trigger processing for the call output
//! (`siri_guard.SiriGuard` and the model-free /s/ detector).
//!
//! `heysiri` mode delays the stream by a short `lookahead` (60 ms, or
//! 230 ms when the region reaches back over the "hey"; derived from the
//! detector, see [`Detector::report_delay`]), runs the detector (an /s/
//! that follows "hey", see [`crate::heysiri`]) on the *undelayed* input, and when it fires ramps the mask `m(t)` to 1 over
//! the reported region (whose start is still inside the delay buffer), then
//! back to 0: `out = dry - m(t) * (dry - wet)`, with `wet` = the notched
//! signal.  `always` mode notches continuously (no detector, no lookahead).

use std::fmt;
use std::str::FromStr;

use anyhow::bail;

use crate::notch::{NotchBank, NotchConfig};
use crate::{DelayLine, Detector, FS};

// --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardMode {
    Always,
    HeySiri,
    Off,
}

impl GuardMode {
    pub const ALL: [GuardMode; 3] = [GuardMode::HeySiri, GuardMode::Always, GuardMode::Off];

    pub fn as_str(self) -> &'static str {
        match self {
            GuardMode::Always => "always",
            GuardMode::HeySiri => "heysiri",
            GuardMode::Off => "off",
        }
    }

    /// Modes that run a detector and need the lookahead.
    pub fn uses_detector(self) -> bool {
        matches!(self, GuardMode::HeySiri)
    }
}

impl FromStr for GuardMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "always" => GuardMode::Always,
            "heysiri" => GuardMode::HeySiri,
            "off" => GuardMode::Off,
            _ => bail!("unknown siri guard mode {s:?}; choose from heysiri, always, off"),
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
    pub ramp_ms: f64,
    pub notch: NotchConfig,
}

/// Safety margin on top of the lookahead the detector needs.
pub const LOOKAHEAD_MARGIN_S: f64 = 0.005;

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            mode: GuardMode::Always,
            ramp_ms: 15.0,
            notch: NotchConfig::default(),
        }
    }
}

/// Streaming processor: `process(block, out)` (same length).
///
/// Latency: notch group delay (12.5 ms default) + lookahead (0 in `always`
/// mode; in `heysiri` mode the detector's report delay + ramp + a 5 ms
/// margin, ~60 ms; `off` passes the input through untouched, latency 0).
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
    clamped: u64,
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
        let ramp = ((cfg.ramp_ms * FS as f64 / 1000.0) as i64).max(1);
        let lookahead = match (&detector, cfg.mode.uses_detector()) {
            (Some(det), true) => Self::required_lookahead(det.as_ref(), ramp as usize),
            _ => 0,
        };
        let latency = if cfg.mode == GuardMode::Off {
            0
        } else {
            notch.delay() + lookahead
        };
        Ok(Self {
            ramp,
            dry_align: DelayLine::new(notch.delay()),
            dry_look: DelayLine::new(lookahead),
            wet_look: DelayLine::new(lookahead),
            notch,
            lookahead,
            detector,
            latency,
            regions: Vec::new(),
            detections: 0,
            clamped: 0,
            t: 0,
            wet: Vec::new(),
            dry: Vec::new(),
            tmp: Vec::new(),
            mask: Vec::new(),
            cfg,
        })
    }

    /// Lookahead (samples) that lets every region reported by `det` start,
    /// ramp included, inside the delay buffer: the detector's worst-case
    /// report delay + the fade-in ramp + [`LOOKAHEAD_MARGIN_S`].
    pub fn required_lookahead(det: &dyn Detector, ramp: usize) -> usize {
        det.report_delay() + ramp + (LOOKAHEAD_MARGIN_S * FS as f64).round() as usize
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

    /// Regions whose start (fade-in ramp included) was already past the
    /// output when reported, so their beginning could not be notched.
    /// Stays 0 when the lookahead covers the detector's report delay.
    pub fn clamped(&self) -> u64 {
        self.clamped
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
                // earliest input index still ahead of (or in) this output block
                let earliest = self.t - self.latency as i64;
                for (a, b) in det.feed(x, self.t) {
                    if a - self.ramp < earliest {
                        self.clamped += 1;
                    }
                    // a region that starts where another ends continues it
                    let extends = self.regions.iter().any(|r| r.1 == a);
                    let a = a.max(earliest);
                    if b > a {
                        self.regions.push((a, b));
                        self.detections += u64::from(!extends);
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
    use crate::heysiri::HeySiriDetector;
    use crate::metrics::{
        align, band_energy_db, db, energy, envelope_correlation, log_spectral_distance,
    };
    use crate::testsig::synth_speech;

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
        for &profile in crate::PROFILES {
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

            // the syllable rhythm survives (most speech energy is below 1 kHz)
            assert!(total > -3.0, "{profile}: {total}");
            assert!(env > 0.8, "{profile}: {env}");
        }
    }

    /// Test helper: fixed regions, reported once they have fully arrived
    /// (like a keyword spotter), with a claimed (possibly understated)
    /// report delay.
    struct OracleDetector {
        regions: Vec<(i64, i64)>,
        given: Vec<bool>,
        claimed_delay: usize,
    }

    impl Detector for OracleDetector {
        fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)> {
            let end = start + x.len() as i64;
            let mut out = Vec::new();
            for (i, &(a, b)) in self.regions.iter().enumerate() {
                if !self.given[i] && b <= end {
                    self.given[i] = true;
                    out.push((a, b));
                }
            }
            out
        }
        fn report_delay(&self) -> usize {
            self.claimed_delay
        }
    }

    #[test]
    fn detection_too_late_is_clamped_not_crashing() {
        let x = synth_speech(3.0, 7);
        let det = OracleDetector {
            regions: vec![((0.2 * FS as f64) as i64, FSI)],
            given: vec![false],
            claimed_delay: 0, // understated: the region is reported 0.8 s late
        };
        let cfg = GuardConfig {
            mode: GuardMode::HeySiri,
            ..Default::default()
        };
        let mut g = SiriGuard::new(cfg, Some(Box::new(det))).unwrap();
        let y = g.process_signal(&x, 480);
        assert_eq!(y.len(), x.len());
        assert!(y.iter().all(|v| v.is_finite()));
        assert_eq!(g.detections(), 1);
        assert_eq!(g.clamped(), 1);
    }

    fn heysiri_guard(det: HeySiriDetector) -> SiriGuard {
        let cfg = GuardConfig {
            mode: GuardMode::HeySiri,
            ..Default::default()
        };
        SiriGuard::new(cfg, Some(Box::new(det))).unwrap()
    }

    #[test]
    fn heysiri_mode_latency_is_derived_from_the_detector() {
        // frame - 1 + pre + ramp (15 ms) + 5 ms margin
        for (det, max_ms) in [
            (HeySiriDetector::default(), 80.0),
            (HeySiriDetector::reaching_back(), 250.0),
        ] {
            let pre = det.pre() as usize;
            let g = heysiri_guard(det);
            let want = crate::heysiri::HOP - 1 + pre + g.ramp() + 240;
            assert_eq!(g.lookahead(), want);
            assert_eq!(g.latency(), 600 + want);
            let ms = g.latency() as f64 / FS as f64 * 1000.0;
            println!(
                "[metrics] siri_guard_heysiri: pre_ms={}, latency_ms={ms:.2}",
                pre / 48
            );
            assert!(ms < max_ms, "{ms} ms");
        }
    }

    #[test]
    fn heysiri_mode_on_speech_never_clamps() {
        let x = synth_speech(8.0, 9);
        for block in [480usize, 97, 1024] {
            for det in [HeySiriDetector::default(), HeySiriDetector::reaching_back()] {
                let mut g = heysiri_guard(det);
                let y = g.process_signal(&x, block);
                assert!(y.iter().all(|v| v.is_finite()));
                assert_eq!(g.clamped(), 0, "block {block}");
            }
        }
    }

    #[test]
    fn modes_and_errors() {
        for m in GuardMode::ALL {
            assert_eq!(m.as_str().parse::<GuardMode>().unwrap(), m);
        }
        assert!("loud".parse::<GuardMode>().is_err());
        let cfg = GuardConfig {
            mode: GuardMode::HeySiri,
            ..Default::default()
        };
        assert!(SiriGuard::new(cfg, None).is_err());
        let g = heysiri_guard(HeySiriDetector::default());
        assert!(g.latency() > 600 && g.latency() < FS / 10);
        // removed modes (the settings map them to heysiri)
        assert!("detect".parse::<GuardMode>().is_err());
        assert!("sibilant".parse::<GuardMode>().is_err());
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
