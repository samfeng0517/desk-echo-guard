//! The boundary between the GUI and the audio program.
//!
//! The GUI only talks to [`Engine`].  Today it is implemented by
//! [`crate::mock::MockEngine`] (fake devices and levels) because
//! `echoguard-core` is being written in parallel.
//!
//! TODO(wiring): add `CoreEngine` implementing [`Engine`] on top of
//! `echoguard-core` (DESIGN.md):
//!   * `start`  -> `EchoGuard::start(&settings, backend.clone(), log)`
//!   * `stop`   -> `EchoGuard::stop()` (bounded join)
//!   * `status` -> `EchoGuard::status()`  (+ `engine_name()`)
//!   * `take_mic_peak` -> `EchoGuard::take_mic_peak()`
//!   * `devices` -> `backend.list_devices()`
//!   * `cable_*` -> `vcable::{status, display_name, resolve, ROLES, run_elevated}`
//!   * `calibrate` -> `core::calibrate(&s, backend, 4.0, level_db, log)`
//!   * `autostart*` -> `winutil` (HKCU Run "DeskEchoGuard")
//!   * `load_settings` / `save_settings` / `default_config_path` -> core settings
//!
//! and replace the mirror types below with re-exports from core
//! (`pub use echoguard_core::{Settings, Status, CableStatus, CableAction}`).
//! The field names here are identical to DESIGN.md, so the GUI code
//! (src/ui/controller.rs) should not need changes beyond that.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use echoguard_audio::{DeviceInfo, DeviceKind};

/// Log lines from the engine (any thread).
pub type LogSink = Arc<dyn Fn(String) + Send + Sync>;

/// Mirror of `echoguard_core::Settings` (same keys and defaults as the
/// Python `app.Settings`).  TODO(wiring): replace with the core type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mic: Option<String>,
    pub speaker: Option<String>,
    pub out: String,
    pub engine: String,
    pub noise_suppression: bool,
    pub high_pass: bool,
    pub agc: bool,
    pub filter_ms: u32,
    pub delay_ms: f64,
    pub mic_delay_ms: f64,
    pub output_gain_db: f64,
    pub ref_buffer_ms: f64,
    pub out_buffer_ms: f64,
    pub max_ppm: f64,
    pub blocksize: usize,
    pub siri_guard: bool,
    pub call_in: String,
    pub siri_mode: String,
    pub siri_model: Vec<String>,
    pub siri_threshold: f64,
    pub siri_profile: String,
    pub siri_depth_db: Option<f64>,
    pub siri_lookahead_s: f64,
    pub spk_buffer_ms: f64,
    pub debug_wav: Option<String>,
    pub debug_max_s: f64,
    pub stats_interval_s: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mic: None,
            speaker: None,
            out: "CABLE Input".into(),
            engine: "auto".into(),
            noise_suppression: true,
            high_pass: true,
            agc: false,
            filter_ms: 300,
            delay_ms: 0.0,
            mic_delay_ms: 0.0,
            output_gain_db: 0.0,
            ref_buffer_ms: 20.0,
            out_buffer_ms: 40.0,
            max_ppm: 1000.0,
            blocksize: 480,
            siri_guard: false,
            call_in: "CABLE-A Output".into(),
            siri_mode: "always".into(),
            siri_model: Vec::new(),
            siri_threshold: 0.5,
            siri_profile: "deep-wide".into(),
            siri_depth_db: None,
            siri_lookahead_s: 1.0,
            spk_buffer_ms: 40.0,
            debug_wav: None,
            debug_max_s: 600.0,
            stats_interval_s: 5.0,
        }
    }
}

/// Mirror of `echoguard_core::Status` (polled by the GUI ~7 times / s).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub mic_db: f32,
    pub ref_db: f32,
    pub out_db: f32,
    pub ref_buf_ms: f32,
    pub drift_ppm: f32,
    pub ref_underruns: u64,
    pub out_underruns: u64,
    pub aec_ms: f32,
    pub siri_detections: u64,
    pub mic_peak: f32,
    pub error: Option<String>,
}

/// Mirror of `echoguard_core::vcable::CableStatus`.
/// `names` keys: "out", "virtual_mic", "call_speaker", "call_in".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CableStatus {
    pub found: bool,
    pub renamed: bool,
    pub names: BTreeMap<&'static str, Option<String>>,
    pub unused: Vec<String>,
}

impl CableStatus {
    /// CABLE-A (VB-CABLE A+B) present: needed by the Hey Siri guard.
    pub fn has_ab(&self) -> bool {
        self.names.get("call_speaker").is_some_and(|n| n.is_some())
            && self.names.get("call_in").is_some_and(|n| n.is_some())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CableAction {
    Rename,
    Hide,
    Restore,
}

/// (original VB-Audio name, our friendly name) of each VB-Cable role, for
/// the "整理 VB-Cable" dialog.  Mirrors `vcable::ROLES`.
pub const CABLE_RENAMES: [(&str, &str); 4] = [
    ("CABLE Output", "回聲消除麥克風"),
    ("CABLE-A Input", "Siri防護喇叭"),
    ("CABLE Input", "EchoGuard 內部輸出（勿選）"),
    ("CABLE-A Output", "EchoGuard 內部輸入（勿選）"),
];
pub const VBCABLE_DOWNLOAD_URL: &str = "https://vb-audio.com/Cable/";

/// Everything the GUI needs from the audio program.
///
/// All methods take `&self` (implementations use interior mutability) so
/// the engine can be shared with worker threads: `start`, `stop`,
/// `calibrate` and `cable_apply_elevated` block and are always called off
/// the UI thread; the rest are cheap and called from the UI thread.
pub trait Engine: Send + Sync {
    // ---- pipeline
    /// Open the devices and start processing (blocking, worker thread).
    fn start(&self, settings: &Settings) -> Result<()>;
    /// Stop processing and join the audio threads (blocking, bounded wait).
    fn stop(&self);
    fn is_running(&self) -> bool;
    /// Live status; `None` while stopped.  Cheap: polled by a UI timer.
    fn status(&self) -> Option<Status>;
    /// Highest mic sample since the last call (clipping warning > 0.9).
    fn take_mic_peak(&self) -> f32;
    /// Name of the AEC engine actually running ("webrtc", ...).
    fn engine_name(&self) -> Option<String>;
    /// Extra latency of the Siri guard, if enabled.
    fn guard_latency_ms(&self) -> Option<f64> {
        None
    }

    // ---- devices / VB-Cable
    fn devices(&self) -> Result<Vec<DeviceInfo>>;
    /// Increments whenever Windows reports a device change (plugged,
    /// removed, renamed); the GUI re-lists devices when it changes.
    fn device_generation(&self) -> u64 {
        0
    }
    fn cable_status(&self, devices: &[DeviceInfo]) -> CableStatus;
    /// Short name to pick in Discord / LINE for role key "virtual_mic" /
    /// "call_speaker" (renamed or original, whichever is present).
    fn cable_display_name(&self, devices: &[DeviceInfo], key: &str) -> String;
    /// Map a configured VB-Cable name to the present device (rename-proof).
    fn cable_resolve(&self, name: &str, kind: DeviceKind, devices: &[DeviceInfo]) -> String;
    /// Rename / hide / restore via a UAC-elevated helper (blocking).
    /// Returns (ok, message).
    fn cable_apply_elevated(&self, actions: &[CableAction]) -> (bool, String);

    // ---- calibration
    /// Play ~4 s of noise, return (delay_ms, confidence) (blocking).
    fn calibrate(&self, settings: &Settings, log: LogSink) -> Result<(f64, f64)>;

    // ---- start with Windows
    fn autostart_supported(&self) -> bool;
    fn autostart_enabled(&self) -> bool;
    fn set_autostart(&self, enabled: bool) -> Result<()>;

    // ---- settings file
    fn default_config_path(&self) -> PathBuf;
    fn load_settings(&self, path: &Path) -> Result<Settings>;
    fn save_settings(&self, path: &Path, settings: &Settings) -> Result<()>;
}

/// `%APPDATA%\desk-echo-guard\echoguard.toml` (Python-compatible path).
pub fn default_config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("desk-echo-guard").join("echoguard.toml")
}
