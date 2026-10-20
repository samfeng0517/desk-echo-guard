//! Runs a 16 kHz engine inside the 48 kHz pipeline (`aec._Resampled`).

use anyhow::{ensure, Result};
use echoguard_dsp::{IntegerResampler, FS};

use crate::{check_frame, EchoCanceller};

/// An engine working on 10 ms blocks at [`Resampled::RATE`] (160 samples).
pub trait LowRateEngine: Send {
    fn name(&self) -> &'static str;
    fn process_lowrate(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> Result<()>;
}

/// 48 kHz -> 16 kHz -> engine -> 48 kHz with streaming polyphase resamplers.
pub struct Resampled<E> {
    engine: E,
    dm: IntegerResampler,
    dr: IntegerResampler,
    up: IntegerResampler,
    low: Vec<f32>,
}

impl<E: LowRateEngine> Resampled<E> {
    pub const RATE: usize = 16_000;

    pub fn new(engine: E) -> Self {
        let k = (FS / Self::RATE) as i32;
        Self {
            engine,
            dm: IntegerResampler::new(k),
            dr: IntegerResampler::new(k),
            up: IntegerResampler::new(-k),
            low: Vec::new(),
        }
    }

    pub fn inner(&self) -> &E {
        &self.engine
    }
}

impl<E: LowRateEngine> EchoCanceller for Resampled<E> {
    fn name(&self) -> &'static str {
        self.engine.name()
    }

    fn process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> Result<()> {
        check_frame(mic, reference, out)?;
        let m = self.dm.process(mic);
        let r = self.dr.process(reference);
        self.low.resize(m.len(), 0.0);
        self.engine.process_lowrate(&m, &r, &mut self.low)?;
        let y = self.up.process(&self.low);
        ensure!(y.len() == out.len(), "resampler produced {} samples", y.len());
        out.copy_from_slice(&y);
        Ok(())
    }
}
