//! Settings, pipeline, calibration, VB-Cable; see `DESIGN.md` (echoguard-core).

pub mod duck;
pub mod pipeline;
pub mod selftest;
pub mod settings;
pub mod vcable;
pub mod winutil;

pub use pipeline::{
    calibrate, make_guard, run_blocking, EchoGuard, LogSink, Status, GUARD_PROFILES,
};
pub use settings::{default_config_path, load_config, save_config, settings_from_table, Settings};
