//! `webrtc` engine: WebRTC APM (AEC3) at 48 kHz (`aec.WebRtcAec`).

use anyhow::{Context, Result};
use echoguard_dsp::{FRAME, FS};
use libwebrtc::native::apm::AudioProcessingModule;

use crate::{check_frame, from_i16, to_i16, EchoCanceller};

pub struct WebRtcAec {
    apm: AudioProcessingModule,
    stream_delay_ms: i32,
    mic: Vec<i16>,
    reference: Vec<i16>,
}

impl WebRtcAec {
    pub fn new(
        noise_suppression: bool,
        high_pass_filter: bool,
        auto_gain_control: bool,
        stream_delay_ms: i32,
    ) -> Self {
        Self {
            apm: AudioProcessingModule::new(true, auto_gain_control, high_pass_filter, noise_suppression),
            stream_delay_ms,
            mic: vec![0; FRAME],
            reference: vec![0; FRAME],
        }
    }
}

impl EchoCanceller for WebRtcAec {
    fn name(&self) -> &'static str {
        "webrtc"
    }

    fn process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> Result<()> {
        check_frame(mic, reference, out)?;
        // same call order as aec.py: reverse stream, stream delay, capture stream
        for (d, &s) in self.reference.iter_mut().zip(reference) {
            *d = to_i16(s);
        }
        self.apm
            .process_reverse_stream(&mut self.reference, FS as i32, 1)
            .context("WebRTC APM process_reverse_stream")?;
        for (d, &s) in self.mic.iter_mut().zip(mic) {
            *d = to_i16(s);
        }
        self.apm
            .set_stream_delay_ms(self.stream_delay_ms)
            .context("WebRTC APM set_stream_delay_ms")?;
        self.apm
            .process_stream(&mut self.mic, FS as i32, 1)
            .context("WebRTC APM process_stream")?;
        for (o, &s) in out.iter_mut().zip(&self.mic) {
            *o = from_i16(s);
        }
        Ok(())
    }
}
