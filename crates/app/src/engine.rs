//! The boundary between the GUI and the audio program.
//!
//! The GUI only talks to [`Engine`].  The real implementation is
//! [`crate::core_engine::CoreEngine`] (echoguard-core + the WASAPI
//! backend); [`crate::mock::MockEngine`] (fake devices and levels) is used
//! for `--screenshot` and with `DEG_MOCK=1`.
//!
//! Mapping to echoguard-core (DESIGN.md):
//!   * `start`  -> `EchoGuard::start(&settings, backend.clone(), log)`
//!   * `stop`   -> `EchoGuard::stop()` (bounded join)
//!   * `status` -> `EchoGuard::status()`  (+ `engine_name()`)
//!   * `take_mic_peak` -> `EchoGuard::take_mic_peak()`
//!   * `devices` -> `backend.list_devices()`
//!   * `cable_*` -> `vcable::{status, display_name, resolve, ROLES, run_elevated}`
//!   * `calibrate` -> `core::calibrate(&s, backend, 4.0, level_db, log)`
//!   * `autostart*` -> `winutil` (HKCU Run "DeskEchoGuard")
//!   * `load_settings` / `save_settings` / `default_config_path` -> core settings

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use echoguard_audio::{DeviceInfo, DeviceKind};
pub use echoguard_core::vcable::{CableAction, CableStatus};
pub use echoguard_core::{LogSink, Settings, Status};

use echoguard_core::vcable::ROLES;

/// GUI helpers on top of the core `CableStatus`.
pub trait CableStatusExt {
    /// CABLE-A (VB-CABLE A+B) present: needed by the Hey Siri guard.
    fn has_ab(&self) -> bool;
}

impl CableStatusExt for CableStatus {
    fn has_ab(&self) -> bool {
        self.names.get("call_speaker").is_some_and(|n| n.is_some())
            && self.names.get("call_in").is_some_and(|n| n.is_some())
    }
}

/// (original VB-Audio name, our friendly name) of each VB-Cable role, for
/// the "整理 VB-Cable" dialog, in the order the dialog lists them.
pub const CABLE_RENAMES: [(&str, &str); 4] = [
    (ROLES[1].original, ROLES[1].renamed), // virtual_mic
    (ROLES[2].original, ROLES[2].renamed), // call_speaker
    (ROLES[0].original, ROLES[0].renamed), // out
    (ROLES[3].original, ROLES[3].renamed), // call_in
];
pub const VBCABLE_DOWNLOAD_URL: &str = echoguard_core::vcable::DOWNLOAD_URL;

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
    echoguard_core::default_config_path()
}

/// GUI settings load: unknown keys are ignored (non-strict), missing keys
/// take their defaults.  The caller falls back to defaults on an error.
pub fn load_settings_file(path: &Path) -> Result<Settings> {
    echoguard_core::settings_from_table(&echoguard_core::load_config(path)?, false)
}

/// Save in the flat, Python-compatible format (via a temp file, so a crash
/// never leaves a half-written settings file).
pub fn save_settings_file(path: &Path, settings: &Settings) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    echoguard_core::save_config(&tmp, settings)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
