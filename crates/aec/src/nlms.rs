//! `nlms` engine: [`Nlms16k`] at 16 kHz (`aec.NlmsAec`).

use anyhow::{ensure, Result};
use echoguard_dsp::nlms::NLMS_BLOCK;
use echoguard_dsp::Nlms16k;

use crate::resampled::{LowRateEngine, Resampled};

pub struct NlmsCore(Nlms16k);

impl LowRateEngine for NlmsCore {
    fn name(&self) -> &'static str {
        "nlms"
    }

    fn process_lowrate(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> Result<()> {
        ensure!(
            mic.len() == NLMS_BLOCK,
            "nlms needs {NLMS_BLOCK}-sample blocks"
        );
        self.0.process(mic, reference, out);
        Ok(())
    }
}

/// Partitioned-block frequency-domain adaptive filter, linear only.
pub type NlmsAec = Resampled<NlmsCore>;

impl NlmsAec {
    pub fn new(filter_ms: u32) -> Self {
        Resampled::wrap(NlmsCore(Nlms16k::new(filter_ms)))
    }
}
