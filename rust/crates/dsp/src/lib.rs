//! DSP core (pure Rust, no OS dependencies).  Port of the numpy/scipy code in
//! `../echoguard`; see `DESIGN.md` for the module map.

pub mod delay_line;
pub mod resample;

pub use delay_line::DelayLine;
pub use resample::IntegerResampler;

/// Internal / device sample rate.
pub const FS: usize = 48_000;
/// 10 ms processing frame.
pub const FRAME: usize = FS / 100;

/// A wake-word style detector for the Siri guard: fed the undelayed call
/// audio block by block, it returns the regions (absolute input-sample
/// indices, `[start, end)`) to notch.  `start` is the index of `x[0]`.
pub trait Detector: Send {
    fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)>;
}
