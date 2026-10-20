//! openWakeWord detector on ONNX Runtime (`siri_guard.OpenWakeWordDetector`);
//! see `DESIGN.md` (echoguard-wakeword).

mod models;
mod oww;

use std::path::PathBuf;

use anyhow::Result;
use echoguard_dsp::{Detector, IntegerResampler, FS};

pub use models::{cache_dir, download, feature_model_paths, EMBEDDING_FILE, MELSPEC_FILE, RELEASE_URL};
pub(crate) use models::reject_tflite;
pub use oww::{warmup_noise, OwwModel, HOP};

const SR: usize = 16_000;

/// Scores one 80 ms hop of 16 kHz int16 audio (one score per label).
/// A trait so the region logic is testable without ONNX.
pub trait HopScorer: Send {
    fn score(&mut self, pcm: &[i16]) -> Result<&[f32]>;
}

impl HopScorer for OwwModel {
    fn score(&mut self, pcm: &[i16]) -> Result<&[f32]> {
        self.predict(pcm)
    }
}

/// 48 kHz block stream -> 16 kHz hops -> scores -> notch regions.
pub struct HopDetector<S: HopScorer> {
    scorer: S,
    threshold: f32,
    pre: i64,
    post: i64,
    refractory: i64,
    down: IntegerResampler,
    acc: Vec<f32>,
    pcm: Vec<i16>,
    last: i64,
    last_scores: Vec<f32>,
    errors: u64,
}

impl<S: HopScorer> HopDetector<S> {
    pub fn new(scorer: S, threshold: f32, pre_s: f64, post_s: f64, refractory_s: f64) -> Self {
        Self {
            scorer,
            threshold,
            pre: (pre_s * FS as f64) as i64,
            post: (post_s * FS as f64) as i64,
            refractory: (refractory_s * FS as f64) as i64,
            down: IntegerResampler::new((FS / SR) as i32),
            acc: Vec::with_capacity(4 * HOP),
            pcm: vec![0; HOP],
            last: -1_000_000_000_000,
            last_scores: Vec::new(),
            errors: 0,
        }
    }

    /// Scores of the most recent hop (Python `last_scores`).
    pub fn last_scores(&self) -> &[f32] {
        &self.last_scores
    }

    /// Number of hops whose scoring failed (skipped, no region).
    pub fn errors(&self) -> u64 {
        self.errors
    }

    pub fn scorer(&self) -> &S {
        &self.scorer
    }

    /// Like `feed`, but also calls `on_hop(t, max_score)` for every hop.
    pub fn feed_with(&mut self, x: &[f32], start: i64, mut on_hop: impl FnMut(i64, f32)) -> Vec<(i64, i64)> {
        let y = self.down.process(x);
        self.acc.extend_from_slice(&y);
        let mut out = Vec::new();
        // approx: detection time = end of this block
        let consumed_end = start + x.len() as i64;
        let mut pos = 0;
        while self.acc.len() - pos >= HOP {
            for (d, &s) in self.pcm.iter_mut().zip(&self.acc[pos..pos + HOP]) {
                // np.clip(x * 32767, -32768, 32767).astype(np.int16)
                *d = (s * 32767.0).clamp(-32768.0, 32767.0) as i16;
            }
            pos += HOP;
            let best = match self.scorer.score(&self.pcm) {
                Ok(s) => {
                    self.last_scores.clear();
                    self.last_scores.extend_from_slice(s);
                    s.iter().copied().fold(0.0f32, f32::max)
                }
                // Python would raise here; we log once and skip the hop
                Err(e) => {
                    if self.errors == 0 {
                        log::error!("喚醒詞偵測失敗：{e:#}");
                    }
                    self.errors += 1;
                    continue;
                }
            };
            let left = (self.acc.len() - pos) as f64;
            let t = consumed_end - (left * FS as f64 / SR as f64) as i64;
            on_hop(t, best);
            if best >= self.threshold && t - self.last > self.refractory {
                self.last = t;
                out.push((t - self.pre, t + self.post));
            }
        }
        self.acc.drain(..pos);
        out
    }
}

impl<S: HopScorer> Detector for HopDetector<S> {
    fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)> {
        self.feed_with(x, start, |_, _| {})
    }
}

/// openWakeWord on a 16 kHz copy of the stream (80 ms hops).  When the score
/// crosses `threshold` at input index t, the phrase is the audio that just
/// ended, so we mark `[t - pre_s, t + post_s]`.
pub struct OpenWakeWordDetector {
    inner: HopDetector<OwwModel>,
}

impl OpenWakeWordDetector {
    /// Python defaults: threshold 0.5, pre 0.9 s, post 0.15 s, refractory 1 s.
    pub fn new(model_paths: &[PathBuf], threshold: f32, pre_s: f64, post_s: f64, refractory_s: f64) -> Result<Self> {
        let (mel, emb) = feature_model_paths(model_paths)?;
        Self::with_feature_models(model_paths, &mel, &emb, threshold, pre_s, post_s, refractory_s)
    }

    /// Like `new`, with explicit feature model paths (no lookup / download).
    pub fn with_feature_models(
        model_paths: &[PathBuf],
        melspec: &std::path::Path,
        embedding: &std::path::Path,
        threshold: f32,
        pre_s: f64,
        post_s: f64,
        refractory_s: f64,
    ) -> Result<Self> {
        let model = OwwModel::new(model_paths, melspec, embedding)?;
        Ok(Self {
            inner: HopDetector::new(model, threshold, pre_s, post_s, refractory_s),
        })
    }

    pub fn last_scores(&self) -> &[f32] {
        self.inner.last_scores()
    }

    pub fn labels(&self) -> &[String] {
        self.inner.scorer().labels()
    }

    pub fn errors(&self) -> u64 {
        self.inner.errors()
    }

    /// Like `feed`, also reporting every hop as `(t, max_score)`.
    pub fn feed_with(&mut self, x: &[f32], start: i64, on_hop: impl FnMut(i64, f32)) -> Vec<(i64, i64)> {
        self.inner.feed_with(x, start, on_hop)
    }
}

impl Detector for OpenWakeWordDetector {
    fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)> {
        self.inner.feed(x, start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scores 1.0 for hops whose first sample is non-zero.
    struct Fake {
        out: [f32; 1],
        hops: usize,
    }
    impl HopScorer for Fake {
        fn score(&mut self, pcm: &[i16]) -> Result<&[f32]> {
            assert_eq!(pcm.len(), HOP);
            self.hops += 1;
            self.out[0] = if pcm.iter().any(|&v| v > 1000) { 1.0 } else { 0.0 };
            Ok(&self.out)
        }
    }

    fn det() -> HopDetector<Fake> {
        HopDetector::new(Fake { out: [0.0], hops: 0 }, 0.5, 0.9, 0.15, 1.0)
    }

    #[test]
    fn hop_timing_and_regions() {
        let mut d = det();
        // 2 s of silence, then a 200 ms loud DC burst, then silence
        let n = 6 * FS;
        let mut x = vec![0.0f32; n];
        x[2 * FS..2 * FS + 9600].iter_mut().for_each(|v| *v = 0.5);
        let mut hops = Vec::new();
        let mut regions = Vec::new();
        for (b, blk) in x.chunks(480).enumerate() {
            let start = (b * 480) as i64;
            regions.extend(d.feed_with(blk, start, |t, s| hops.push((t, s, start + 480))));
        }
        // one hop per 3840 input samples, t = end of the block that completed it
        assert_eq!(hops.len(), n / 3840);
        for (i, &(t, _, end)) in hops.iter().enumerate() {
            assert_eq!(end, ((i + 1) * 3840) as i64, "hop {i} completes at a block end");
            assert_eq!(t, end);
        }
        // burst (plus resampler delay) spans several hops: one region only (refractory)
        let hits: Vec<_> = hops.iter().filter(|h| h.1 >= 0.5).collect();
        assert!(hits.len() >= 2, "{hits:?}");
        assert_eq!(regions.len(), 1);
        let t = hits[0].0;
        assert_eq!(regions[0], (t - 43_200, t + 7_200));
    }

    #[test]
    fn refractory_and_partial_hops() {
        let mut d = det();
        // block sizes that do not divide 3840: t is the block end minus the
        // leftover 16 kHz samples scaled to 48 kHz
        let mut x = vec![0.0f32; 8 * FS];
        for k in [FS, FS + 2 * FS / 3, 4 * FS] {
            x[k..k + 960].iter_mut().for_each(|v| *v = 0.9);
        }
        let mut regions = Vec::new();
        let mut ts = Vec::new();
        let mut pos = 0usize;
        for (i, len) in [1500usize, 3000, 4800, 960, 7200].iter().cycle().enumerate() {
            if pos >= x.len() {
                break;
            }
            let len = (*len).min(x.len() - pos);
            let start = pos as i64;
            regions.extend(d.feed_with(&x[pos..pos + len], start, |t, s| {
                assert!(t <= start + len as i64 && t > start - 3840, "hop {i}");
                ts.push((t, s));
            }));
            pos += len;
        }
        // hop times strictly increase by 3840
        for w in ts.windows(2) {
            assert_eq!(w[1].0 - w[0].0, 3840);
        }
        // burst at 1 s and 1.67 s are within the 1 s refractory: one region; 4 s: another
        assert_eq!(regions.len(), 2, "{regions:?}");
        assert!(regions[1].0 - regions[0].0 > FS as i64);
        assert_eq!(d.scorer().hops, ts.len());
    }

    #[test]
    fn scorer_errors_skip_hop() {
        struct Bad;
        impl HopScorer for Bad {
            fn score(&mut self, _: &[i16]) -> Result<&[f32]> {
                anyhow::bail!("boom")
            }
        }
        let mut d = HopDetector::new(Bad, 0.0, 0.9, 0.15, 1.0);
        let r = d.feed(&[0.0; 7680], 0);
        assert!(r.is_empty());
        assert_eq!(d.errors(), 2);
    }

    #[test]
    fn tflite_rejected() {
        let e = feature_model_paths(&[PathBuf::from("hey_siri.tflite")]).unwrap_err();
        assert!(e.to_string().contains(".onnx"), "{e}");
    }
}
