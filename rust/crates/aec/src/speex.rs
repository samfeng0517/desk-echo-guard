//! `speex` engine: SpeexDSP echo canceller + preprocessor at 16 kHz, set up
//! exactly like the `pyaec` wheel (aec-rs): `speex_echo_state_init(160,
//! filter_len)` (sampling rate left at SpeexDSP's default, as there), and
//! with `preprocess` a preprocessor at 16 kHz linked via
//! `SPEEX_PREPROCESS_SET_ECHO_STATE`; per block echo cancellation, then
//! `speex_preprocess_run` on the output.

use std::os::raw::{c_int, c_void};

use anyhow::{bail, ensure, Result};

use crate::resampled::{LowRateEngine, Resampled};
use crate::{from_i16, to_i16};

#[repr(C)]
struct SpeexEchoState {
    _p: [u8; 0],
}
#[repr(C)]
struct SpeexPreprocessState {
    _p: [u8; 0],
}

const SPEEX_PREPROCESS_SET_ECHO_STATE: c_int = 24;

extern "C" {
    fn speex_echo_state_init(frame_size: c_int, filter_length: c_int) -> *mut SpeexEchoState;
    fn speex_echo_state_destroy(st: *mut SpeexEchoState);
    fn speex_echo_cancellation(
        st: *mut SpeexEchoState,
        rec: *const i16,
        play: *const i16,
        out: *mut i16,
    );
    fn speex_preprocess_state_init(
        frame_size: c_int,
        sampling_rate: c_int,
    ) -> *mut SpeexPreprocessState;
    fn speex_preprocess_state_destroy(st: *mut SpeexPreprocessState);
    fn speex_preprocess_ctl(
        st: *mut SpeexPreprocessState,
        request: c_int,
        ptr: *mut c_void,
    ) -> c_int;
    fn speex_preprocess_run(st: *mut SpeexPreprocessState, x: *mut i16) -> c_int;
}

const RATE: usize = 16_000;
const N: usize = RATE / 100;

pub struct SpeexCore {
    echo: *mut SpeexEchoState,
    pre: *mut SpeexPreprocessState,
    mic: [i16; N],
    reference: [i16; N],
    out: [i16; N],
}

// The states are owned exclusively by this value and never shared.
unsafe impl Send for SpeexCore {}

impl SpeexCore {
    fn new(filter_ms: u32, preprocess: bool) -> Result<Self> {
        // int(rate * filter_ms / 1000) as in aec.py
        let filter_len = (RATE as u64 * filter_ms as u64 / 1000).max(1);
        ensure!(filter_len <= i32::MAX as u64, "filter_ms too large");
        // SAFETY: plain constructor calls; results checked for NULL below.
        let echo = unsafe { speex_echo_state_init(N as c_int, filter_len as c_int) };
        if echo.is_null() {
            bail!("SpeexDSP echo state init failed");
        }
        let mut s = Self {
            echo,
            pre: std::ptr::null_mut(),
            mic: [0; N],
            reference: [0; N],
            out: [0; N],
        };
        if preprocess {
            // SAFETY: as above; `echo` is a valid state that outlives `pre`
            // (both destroyed together in Drop, preprocessor first).
            unsafe {
                s.pre = speex_preprocess_state_init(N as c_int, RATE as c_int);
                if s.pre.is_null() {
                    bail!("SpeexDSP preprocess state init failed");
                }
                speex_preprocess_ctl(s.pre, SPEEX_PREPROCESS_SET_ECHO_STATE, s.echo.cast());
            }
        }
        Ok(s)
    }
}

impl Drop for SpeexCore {
    fn drop(&mut self) {
        // SAFETY: pointers come from the matching init calls and are freed once.
        unsafe {
            if !self.pre.is_null() {
                speex_preprocess_state_destroy(self.pre);
            }
            speex_echo_state_destroy(self.echo);
        }
    }
}

impl LowRateEngine for SpeexCore {
    fn name(&self) -> &'static str {
        "speex"
    }

    fn process_lowrate(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> Result<()> {
        ensure!(
            mic.len() == N && reference.len() == N && out.len() == N,
            "speex needs {N}-sample blocks"
        );
        for (d, &s) in self.mic.iter_mut().zip(mic) {
            *d = to_i16(s);
        }
        for (d, &s) in self.reference.iter_mut().zip(reference) {
            *d = to_i16(s);
        }
        // SAFETY: valid states, all buffers hold exactly N = frame_size samples.
        unsafe {
            speex_echo_cancellation(
                self.echo,
                self.mic.as_ptr(),
                self.reference.as_ptr(),
                self.out.as_mut_ptr(),
            );
            if !self.pre.is_null() {
                speex_preprocess_run(self.pre, self.out.as_mut_ptr());
            }
        }
        for (o, &s) in out.iter_mut().zip(&self.out) {
            *o = from_i16(s);
        }
        Ok(())
    }
}

/// SpeexDSP AEC (+ residual echo suppression / denoise when `preprocess`).
pub type SpeexAec = Resampled<SpeexCore>;

impl SpeexAec {
    pub fn new(filter_ms: u32, preprocess: bool) -> Result<Self> {
        Ok(Resampled::wrap(SpeexCore::new(filter_ms, preprocess)?))
    }
}
