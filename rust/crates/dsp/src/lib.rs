//! DSP core (pure Rust, no OS dependencies).  Port of the numpy/scipy code in
//! `../echoguard`; see `DESIGN.md` for the module map.

pub mod delay_est;
pub mod delay_line;
pub mod elastic;
pub mod filters;
pub mod guard;
pub mod metrics;
pub mod nlms;
pub mod notch;
pub mod resample;
pub mod testsig;

pub use delay_est::estimate_delay_ms;
pub use delay_line::DelayLine;
pub use elastic::{Clock, ElasticBuffer, ElasticOptions, ElasticStats};
pub use guard::{GuardConfig, GuardMode, OracleDetector, SibilantDetector, SiriGuard};
pub use metrics::{band_energy_db, best_lag, db, energy, erle_db};
pub use nlms::Nlms16k;
pub use notch::{design_notch_fir, notch_target_gain, NotchBank, NotchConfig, DEFAULT_PROFILE, PROFILES};
pub use resample::IntegerResampler;
pub use testsig::{make_scenario, synth_speech, Scenario};

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
