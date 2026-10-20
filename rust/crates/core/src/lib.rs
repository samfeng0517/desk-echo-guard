//! Settings, pipeline, calibration, VB-Cable; see `DESIGN.md` (echoguard-core).

pub mod settings;
pub mod vcable;
pub mod winutil;

pub use settings::{default_config_path, load_config, save_config, settings_from_table, Settings};
