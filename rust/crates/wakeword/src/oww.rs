//! Port of openWakeWord's `Model` + `AudioFeatures` streaming pipeline
//! (openwakeword 0.6, ONNX framework) on tract (pure-Rust ONNX inference).
//!
//! 16 kHz int16 audio -> melspectrogram.onnx (`x / 10 + 2`) -> 76-frame
//! windows, step 8 -> embedding_model.onnx (96-dim) -> the last N embedding
//! frames -> wake-word model(s).  Buffer sizes, remainder handling and the
//! "first 5 predictions are zero" rule follow the Python code exactly.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use tract_onnx::prelude::*;

/// Samples per openWakeWord hop (80 ms @ 16 kHz).
pub const HOP: usize = 1280;
const MEL_BINS: usize = 32;
const EMB_DIM: usize = 96;
const WINDOW: usize = 76;
const RAW_MAX: usize = 16_000 * 10;
const MEL_MAX_ROWS: usize = 10 * 97;
const FEAT_MAX_ROWS: usize = 120;
const PRED_BUF_LEN: usize = 30;

type Plan = TypedSimplePlan;

/// An optimised tract plan plus its reusable run state (single-threaded,
/// pure Rust).
pub(crate) struct Session {
    state: TypedSimpleState,
    input: TypedFact,
    output: TypedFact,
}

impl Session {
    /// Load `path`; `input` overrides the first input's shape (`None`: keep
    /// the model's own, which must then be fully known).  `"B"`-style
    /// symbolic dims are resolved per run.
    fn load(path: &Path, input: Option<&[&str]>) -> Result<Self> {
        let r = (|| -> TractResult<Self> {
            let mut model = tract_onnx::onnx().model_for_path(path)?;
            if let Some(dims) = input {
                let shape: TVec<TDim> = dims
                    .iter()
                    .map(|d| match d.parse::<i64>() {
                        Ok(v) => TDim::Val(v),
                        Err(_) => TDim::Sym(model.sym(d)),
                    })
                    .collect();
                model.set_input_fact(0, f32::fact(shape).into())?;
            }
            let model = model.into_optimized()?;
            let input = model.input_fact(0)?.clone();
            let output = model.output_fact(0)?.clone();
            let plan: Arc<Plan> = model.into_runnable()?;
            Ok(Self {
                state: plan.spawn()?,
                input,
                output,
            })
        })();
        r.with_context(|| format!("無法載入 ONNX 模型：{}", path.display()))
    }

    /// Input dims with unknown (symbolic) ones as -1.
    fn input_dims(&self) -> Vec<i64> {
        dims(&self.input)
    }

    fn output_dims(&self) -> Vec<i64> {
        dims(&self.output)
    }

    /// Run on an f32 tensor; the first output is written to `out` (cleared
    /// first).
    fn run_into(&mut self, shape: &[usize], data: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let input = Tensor::from_shape(shape, data)?;
        let res = self.state.run(tvec!(input.into_tvalue()))?;
        let v = res
            .first()
            .ok_or_else(|| anyhow!("模型沒有輸出"))?
            .to_plain_array_view::<f32>()?;
        out.clear();
        out.extend(v.iter().copied());
        Ok(())
    }
}

fn dims(f: &TypedFact) -> Vec<i64> {
    f.shape
        .iter()
        .map(|d| d.to_i64().unwrap_or(-1))
        .collect()
}

/// Deterministic stand-in for openWakeWord's random warm-up audio
/// (`np.random.randint(-1000, 1000, 16000*4)`).  Mirrored by
/// tests/fixtures/gen_reference.py.
pub fn warmup_noise(n: usize) -> Vec<i16> {
    let mut s: u64 = 0x5EED;
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 33) % 2000) as i16 - 1000
        })
        .collect()
}

struct WakeModel {
    name: String,
    session: Session,
    /// number of embedding frames the model takes (input shape [1, N, 96])
    n_in: usize,
    n_out: usize,
    /// per output class: (len of prediction_buffer (capped at 30), last value)
    history: Vec<(usize, f32)>,
}

/// openWakeWord `Model` restricted to what echoguard uses (no VAD, no Speex
/// noise suppression, no custom verifiers, no patience / debounce).
pub struct OwwModel {
    mel: Session,
    emb: Session,
    models: Vec<WakeModel>,
    raw: Vec<f32>,
    remainder: Vec<i16>,
    accumulated: usize,
    mel_buf: Vec<f32>,
    feat_buf: Vec<f32>,
    scores: Vec<f32>,
    labels: Vec<String>,
    tmp: Vec<f32>,
}

impl OwwModel {
    pub fn new(
        model_paths: &[std::path::PathBuf],
        melspec: &Path,
        embedding: &Path,
    ) -> Result<Self> {
        if model_paths.is_empty() {
            bail!("沒有指定喚醒詞模型（.onnx）");
        }
        crate::reject_tflite(model_paths)?;
        let mut models = Vec::new();
        let mut labels = Vec::new();
        for p in model_paths {
            let session = Session::load(p, None)?;
            let (din, dout) = (session.input_dims(), session.output_dims());
            if din.len() != 3 || din[1] <= 0 || din[2] != EMB_DIM as i64 {
                bail!(
                    "不支援的 openWakeWord 模型 {}：輸入形狀 {:?}（預期 [1, N, 96]）",
                    p.display(),
                    din
                );
            }
            let n_out = if dout.len() >= 2 && dout[1] > 0 {
                dout[1] as usize
            } else {
                1
            };
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if n_out == 1 {
                labels.push(name.clone());
            } else {
                labels.extend((0..n_out).map(|c| c.to_string()));
            }
            models.push(WakeModel {
                name,
                session,
                n_in: din[1] as usize,
                n_out,
                history: vec![(0, 0.0); n_out],
            });
        }
        let mut me = Self {
            mel: Session::load(melspec, Some(&["1", "S"]))?,
            emb: Session::load(embedding, Some(&["B", "76", "32", "1"]))?,
            models,
            raw: Vec::with_capacity(2 * RAW_MAX),
            remainder: Vec::new(),
            accumulated: 0,
            mel_buf: vec![1.0; WINDOW * MEL_BINS],
            feat_buf: Vec::with_capacity((FEAT_MAX_ROWS + 64) * EMB_DIM),
            scores: Vec::new(),
            labels,
            tmp: Vec::new(),
        };
        me.feat_buf = me.embed_clip(&warmup_noise(16_000 * 4))?;
        Ok(me)
    }

    /// Score labels, in the order of `predict`'s result (model file stem for
    /// single-output models, class index for multi-class ones).
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Model names (file stems).
    pub fn model_names(&self) -> Vec<&str> {
        self.models.iter().map(|m| m.name.as_str()).collect()
    }

    /// `Model.predict(x)`: feed any number of 16 kHz samples, get one score
    /// per label.
    pub fn predict(&mut self, x: &[i16]) -> Result<&[f32]> {
        let n = self.streaming_features(x)?;
        self.scores.clear();
        let rows = self.feat_buf.len() / EMB_DIM;
        for m in &mut self.models {
            let mut pred = vec![f32::NEG_INFINITY; m.n_out];
            if n >= HOP {
                // n > 1280: max over the predictions for every new hop
                for i in (0..n / HOP).rev() {
                    let end = rows
                        .checked_sub(i)
                        .filter(|&e| e >= m.n_in)
                        .ok_or_else(|| anyhow!("特徵緩衝區不足 {} 幀", m.n_in))?;
                    let feats = &self.feat_buf[(end - m.n_in) * EMB_DIM..end * EMB_DIM];
                    let out = &mut self.tmp;
                    m.session.run_into(&[1, m.n_in, EMB_DIM], feats, out)?;
                    if out.len() < m.n_out {
                        bail!("模型 {} 輸出長度 {}，預期 {}", m.name, out.len(), m.n_out);
                    }
                    for (p, &o) in pred.iter_mut().zip(out.iter()) {
                        *p = p.max(o);
                    }
                }
            } else if m.n_out == 1 {
                // not enough samples yet: repeat the previous prediction
                pred[0] = if m.history[0].0 > 0 {
                    m.history[0].1
                } else {
                    0.0
                };
            } else {
                pred.iter_mut().for_each(|p| *p = 0.0);
            }
            for (p, h) in pred.iter_mut().zip(&m.history) {
                if h.0 < 5 {
                    *p = 0.0;
                }
            }
            self.scores.extend_from_slice(&pred);
        }
        let mut k = 0;
        for m in &mut self.models {
            for h in &mut m.history {
                *h = ((h.0 + 1).min(PRED_BUF_LEN), self.scores[k]);
                k += 1;
            }
        }
        Ok(&self.scores)
    }

    /// `AudioFeatures._streaming_features`; returns the number of samples
    /// that were turned into features (or the accumulated count).
    fn streaming_features(&mut self, x: &[i16]) -> Result<usize> {
        let joined;
        let x: &[i16] = if self.remainder.is_empty() {
            x
        } else {
            let mut v = std::mem::take(&mut self.remainder);
            v.extend_from_slice(x);
            joined = v;
            &joined
        };
        if self.accumulated + x.len() >= HOP {
            let rem = (self.accumulated + x.len()) % HOP;
            let even = &x[..x.len() - rem];
            self.buffer_raw(even);
            self.accumulated += even.len();
            self.remainder = x[x.len() - rem..].to_vec();
        } else {
            self.accumulated += x.len();
            self.buffer_raw(x);
        }

        let mut processed = 0;
        if self.accumulated >= HOP && self.accumulated.is_multiple_of(HOP) {
            self.streaming_melspectrogram(self.accumulated)?;
            let rows = self.mel_buf.len() / MEL_BINS;
            for i in (0..self.accumulated / HOP).rev() {
                if rows < WINDOW + 8 * i {
                    continue; // Python's slice comes out short and is skipped
                }
                let end = rows - 8 * i;
                let win = &self.mel_buf[(end - WINDOW) * MEL_BINS..end * MEL_BINS];
                self.emb.run_into(&[1, WINDOW, MEL_BINS, 1], win, &mut self.tmp)?;
                if self.tmp.len() != EMB_DIM {
                    bail!("embedding 模型輸出長度 {}，預期 {EMB_DIM}", self.tmp.len());
                }
                self.feat_buf.extend_from_slice(&self.tmp);
            }
            processed = self.accumulated;
            self.accumulated = 0;
        }
        trim_rows(&mut self.feat_buf, EMB_DIM, FEAT_MAX_ROWS);
        Ok(if processed != 0 {
            processed
        } else {
            self.accumulated
        })
    }

    fn buffer_raw(&mut self, x: &[i16]) {
        self.raw.extend(x.iter().map(|&v| v as f32));
        if self.raw.len() > 2 * RAW_MAX {
            let cut = self.raw.len() - RAW_MAX;
            self.raw.drain(..cut);
        }
    }

    fn streaming_melspectrogram(&mut self, n: usize) -> Result<()> {
        // deque(maxlen=160000)[-n-480:]
        let avail = self.raw.len().min(RAW_MAX);
        let take = (n + 3 * 160).min(avail);
        let audio = &self.raw[self.raw.len() - take..];
        self.mel.run_into(&[1, take], audio, &mut self.tmp)?;
        if !self.tmp.len().is_multiple_of(MEL_BINS) {
            bail!(
                "melspectrogram 模型輸出長度 {} 不是 32 的倍數",
                self.tmp.len()
            );
        }
        self.mel_buf
            .extend(self.tmp.iter().map(|&v| v / 10.0 + 2.0));
        trim_rows(&mut self.mel_buf, MEL_BINS, MEL_MAX_ROWS);
        Ok(())
    }

    /// `AudioFeatures._get_embeddings` for a whole clip (batched).
    fn embed_clip(&mut self, x: &[i16]) -> Result<Vec<f32>> {
        let audio: Vec<f32> = x.iter().map(|&v| v as f32).collect();
        let mut spec = Vec::new();
        self.mel.run_into(&[1, audio.len()], &audio, &mut spec)?;
        spec.iter_mut().for_each(|v| *v = *v / 10.0 + 2.0);
        let frames = spec.len() / MEL_BINS;
        let mut batch = Vec::new();
        let mut b = 0;
        for i in (0..frames).step_by(8) {
            if i + WINDOW <= frames {
                batch.extend_from_slice(&spec[i * MEL_BINS..(i + WINDOW) * MEL_BINS]);
                b += 1;
            }
        }
        if b == 0 {
            bail!("warm-up 音訊太短");
        }
        let mut out = Vec::new();
        self.emb.run_into(&[b, WINDOW, MEL_BINS, 1], &batch, &mut out)?;
        if out.len() != b * EMB_DIM {
            bail!("embedding 模型輸出長度 {}，預期 {}", out.len(), b * EMB_DIM);
        }
        Ok(out)
    }
}

/// Keep only the last `max_rows` rows of a row-major buffer.
fn trim_rows(buf: &mut Vec<f32>, width: usize, max_rows: usize) {
    if buf.len() > max_rows * width {
        let cut = buf.len() - max_rows * width;
        buf.drain(..cut);
    }
}
