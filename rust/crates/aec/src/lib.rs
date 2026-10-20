//! Acoustic echo cancellation engines behind one interface (`aec.py`).
//!
//! All engines take 10 ms frames at 48 kHz (mono `f32`, roughly -1..1) for the
//! microphone (near end + echo) and the speaker reference (far end) and
//! return the cleaned 10 ms mic frame at 48 kHz.  Engines that run at 16 kHz
//! are wrapped with streaming resamplers.
//!
//! * `webrtc`: WebRTC AudioProcessing (AEC3 + optional NS / HPF / AGC) via
//!   LiveKit's `libwebrtc`.  Best quality.  Feature `webrtc`.
//! * `speex`: SpeexDSP echo canceller + preprocessor (residual echo
//!   suppression), configured like the `pyaec` wheel.  Feature `speex`.
//! * `nlms`: built-in partitioned-block frequency-domain adaptive filter
//!   ([`echoguard_dsp::Nlms16k`]).  Linear only.
//! * `off`: passthrough.

use anyhow::{bail, ensure, Result};
use echoguard_dsp::{DelayLine, FRAME, FS};

mod nlms;
mod resampled;
#[cfg(feature = "speex")]
mod speex;
#[cfg(feature = "webrtc")]
mod webrtc;

pub use nlms::NlmsAec;
pub use resampled::{LowRateEngine, Resampled};
#[cfg(feature = "speex")]
pub use speex::SpeexAec;
#[cfg(feature = "webrtc")]
pub use webrtc::WebRtcAec;

/// Engine settings (`make_engine` keyword arguments).
#[derive(Clone, Debug, PartialEq)]
pub struct EngineOptions {
    pub noise_suppression: bool,
    pub high_pass_filter: bool,
    pub auto_gain_control: bool,
    /// Adaptive filter length for speex / nlms.
    pub filter_ms: u32,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            noise_suppression: true,
            high_pass_filter: true,
            auto_gain_control: false,
            filter_ms: 300,
        }
    }
}

pub trait EchoCanceller: Send {
    fn name(&self) -> &'static str;
    /// 10 ms @ 48 kHz in, 10 ms @ 48 kHz out
    fn process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> Result<()>;
}

/// All engine names, best first.
pub const ENGINES: &[&str] = &["webrtc", "speex", "nlms", "off"];

/// Engines compiled into this build, best first.
pub fn available_engines() -> Vec<&'static str> {
    let mut v = Vec::with_capacity(4);
    if cfg!(feature = "webrtc") {
        v.push("webrtc");
    }
    if cfg!(feature = "speex") {
        v.push("speex");
    }
    v.extend(["nlms", "off"]);
    v
}

/// Create an engine by name; `"auto"` = the first available one.
pub fn make_engine(name: &str, opts: &EngineOptions) -> Result<Box<dyn EchoCanceller>> {
    let name = if name == "auto" { available_engines()[0] } else { name };
    match name {
        #[cfg(feature = "webrtc")]
        "webrtc" => Ok(Box::new(WebRtcAec::new(
            opts.noise_suppression,
            opts.high_pass_filter,
            opts.auto_gain_control,
            0,
        ))),
        #[cfg(feature = "speex")]
        "speex" => Ok(Box::new(SpeexAec::new(opts.filter_ms, opts.noise_suppression)?)),
        "nlms" => Ok(Box::new(NlmsAec::new(opts.filter_ms))),
        "off" => Ok(Box::new(PassThrough)),
        n if ENGINES.contains(&n) => bail!("此版本未內建回音消除引擎 {n:?}"),
        n => bail!("未知的回音消除引擎 {n:?}；可選：{}", ENGINES.join(", ")),
    }
}

/// `off`: mic passes through unchanged.
pub struct PassThrough;

impl EchoCanceller for PassThrough {
    fn name(&self) -> &'static str {
        "off"
    }

    fn process(&mut self, mic: &[f32], _reference: &[f32], out: &mut [f32]) -> Result<()> {
        ensure!(out.len() == mic.len(), "output length must match the mic frame");
        out.copy_from_slice(mic);
        Ok(())
    }
}

/// `aec._to_i16`: round half to even (like numpy), clip.
#[inline]
pub(crate) fn to_i16(x: f32) -> i16 {
    (x * 32767.0).round_ties_even().clamp(-32768.0, 32767.0) as i16
}

/// `aec._from_i16`
#[inline]
pub(crate) fn from_i16(x: i16) -> f32 {
    x as f32 / 32768.0
}

pub(crate) fn check_frame(mic: &[f32], reference: &[f32], out: &[f32]) -> Result<()> {
    ensure!(
        mic.len() == FRAME && reference.len() == FRAME && out.len() == FRAME,
        "AEC needs 10 ms @ 48 kHz frames ({FRAME} samples), got mic {} / ref {} / out {}",
        mic.len(),
        reference.len(),
        out.len()
    );
    Ok(())
}

/// Frame-level mic pipeline: optional fixed delays + AEC engine + gain.
///
/// `ref_delay_ms` delays the reference (echo arrives in the mic much later
/// than the reference from loopback); `mic_delay_ms` delays the mic (the
/// reference arrives *later* than its echo, which no causal canceller can
/// handle).
pub struct AecProcessor {
    engine: Box<dyn EchoCanceller>,
    ref_delay: DelayLine,
    mic_delay: DelayLine,
    gain: f32,
    mic_buf: Vec<f32>,
    ref_buf: Vec<f32>,
}

impl AecProcessor {
    pub fn new(
        engine: Box<dyn EchoCanceller>,
        ref_delay_ms: f64,
        mic_delay_ms: f64,
        output_gain_db: f64,
    ) -> Self {
        let samples = |ms: f64| (ms * FS as f64 / 1000.0).round_ties_even().max(0.0) as usize;
        Self {
            engine,
            ref_delay: DelayLine::new(samples(ref_delay_ms)),
            mic_delay: DelayLine::new(samples(mic_delay_ms)),
            gain: 10f64.powf(output_gain_db / 20.0) as f32,
            mic_buf: vec![0.0; FRAME],
            ref_buf: vec![0.0; FRAME],
        }
    }

    /// One 10 ms frame at 48 kHz.
    pub fn process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> Result<()> {
        check_frame(mic, reference, out)?;
        self.mic_delay.process(mic, &mut self.mic_buf);
        self.ref_delay.process(reference, &mut self.ref_buf);
        self.engine.process(&self.mic_buf, &self.ref_buf, out)?;
        let g = self.gain;
        for v in out.iter_mut() {
            if g != 1.0 {
                *v *= g;
            }
            *v = v.clamp(-1.0, 1.0);
        }
        Ok(())
    }

    /// Offline helper: whole signals, truncated to a multiple of 10 ms.
    pub fn process_signal(&mut self, mic: &[f32], reference: &[f32]) -> Result<Vec<f32>> {
        let n = mic.len().min(reference.len()) / FRAME * FRAME;
        let mut out = vec![0.0; n];
        for s in (0..n).step_by(FRAME) {
            let e = s + FRAME;
            self.process(&mic[s..e], &reference[s..e], &mut out[s..e])?;
        }
        Ok(out)
    }

    pub fn engine_name(&self) -> &'static str {
        self.engine.name()
    }
}

#[cfg(test)]
mod tests;
