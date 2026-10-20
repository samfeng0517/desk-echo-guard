//! The real [`Engine`]: echoguard-core on the platform audio backend
//! (WASAPI on Windows).  Elsewhere there is no backend: every audio call
//! fails with a plain-words error, which the GUI shows in an InfoBar.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Result};
use echoguard_audio::AudioBackend;
use echoguard_core::{vcable, winutil, EchoGuard};
use parking_lot::Mutex;

use crate::engine::*;

/// How often the device watcher re-lists the devices.
const DEVICE_POLL: Duration = Duration::from_secs(3);
/// Calibration: length and level of the noise bursts (as the Python GUI).
const CALIBRATE_S: f64 = 4.0;
const CALIBRATE_LEVEL_DB: f64 = -20.0;

/// Background thread that bumps a counter whenever the device list changes.
/// Stops when dropped.
struct DeviceWatcher {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl DeviceWatcher {
    fn start(backend: Arc<dyn AudioBackend>, generation: Arc<AtomicU64>, every: Duration) -> Self {
        let (tx, rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("device-watch".into())
            .spawn(move || {
                let mut last = backend.list_devices().ok();
                loop {
                    match rx.recv_timeout(every) {
                        Err(RecvTimeoutError::Timeout) => {}
                        // stop requested (sender dropped or sent)
                        _ => return,
                    }
                    let now = backend.list_devices().ok();
                    if now != last {
                        last = now;
                        generation.fetch_add(1, Relaxed);
                    }
                }
            })
            .ok();
        DeviceWatcher {
            stop: Some(tx),
            thread,
        }
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct CoreEngine {
    /// The audio backend, or why there is none (shown to the user).
    backend: std::result::Result<Arc<dyn AudioBackend>, String>,
    log: LogSink,
    guard: Mutex<Option<EchoGuard>>,
    /// Serialises start / stop (both run on worker threads) without holding
    /// `guard` while devices open, so `status` never blocks the UI.
    op: Mutex<()>,
    generation: Arc<AtomicU64>,
    _watcher: Option<DeviceWatcher>,
}

impl CoreEngine {
    /// On the platform's default backend (WASAPI on Windows).
    pub fn new(log: LogSink) -> Self {
        let backend = echoguard_audio::default_backend().map_err(|e| format!("{e:#}"));
        Self::build(backend, log, DEVICE_POLL)
    }

    /// On a given backend (tests: `SimBackend`).
    #[allow(dead_code)] // only the tests inject a backend today
    pub fn with_backend(backend: Arc<dyn AudioBackend>, log: LogSink) -> Self {
        Self::build(Ok(backend), log, DEVICE_POLL)
    }

    fn build(
        backend: std::result::Result<Arc<dyn AudioBackend>, String>,
        log: LogSink,
        every: Duration,
    ) -> Self {
        let generation = Arc::new(AtomicU64::new(0));
        let watcher = backend
            .as_ref()
            .ok()
            .map(|b| DeviceWatcher::start(b.clone(), generation.clone(), every));
        CoreEngine {
            backend,
            log,
            guard: Mutex::new(None),
            op: Mutex::new(()),
            generation,
            _watcher: watcher,
        }
    }

    fn backend(&self) -> Result<Arc<dyn AudioBackend>> {
        self.backend.clone().map_err(|e| anyhow!(e))
    }

    /// Take the running pipeline out and stop it (caller holds `op`).
    fn stop_locked(&self) {
        let eg = self.guard.lock().take();
        if let Some(eg) = eg {
            if let Err(e) = eg.stop() {
                (self.log)(format!("停止時發生錯誤：{e:#}"));
            }
        }
    }
}

impl Drop for CoreEngine {
    fn drop(&mut self) {
        let _op = self.op.lock();
        self.stop_locked();
    }
}

impl Engine for CoreEngine {
    fn start(&self, settings: &Settings) -> Result<()> {
        let _op = self.op.lock();
        self.stop_locked();
        let eg = EchoGuard::start(settings, self.backend()?, self.log.clone())?;
        *self.guard.lock() = Some(eg);
        Ok(())
    }

    fn stop(&self) {
        let _op = self.op.lock();
        self.stop_locked();
    }

    fn is_running(&self) -> bool {
        self.guard.lock().is_some()
    }

    fn status(&self) -> Option<Status> {
        self.guard.lock().as_ref().map(EchoGuard::status)
    }

    fn take_mic_peak(&self) -> f32 {
        self.guard
            .lock()
            .as_ref()
            .map_or(0.0, EchoGuard::take_mic_peak)
    }

    fn engine_name(&self) -> Option<String> {
        self.guard
            .lock()
            .as_ref()
            .map(|g| g.engine_name().to_string())
    }

    fn guard_latency_ms(&self) -> Option<f64> {
        self.guard
            .lock()
            .as_ref()
            .and_then(EchoGuard::guard_latency_ms)
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>> {
        self.backend()?.list_devices()
    }

    fn device_generation(&self) -> u64 {
        self.generation.load(Relaxed)
    }

    fn cable_status(&self, devices: &[DeviceInfo]) -> CableStatus {
        vcable::status(devices)
    }

    fn cable_display_name(&self, devices: &[DeviceInfo], key: &str) -> String {
        vcable::display_name(devices, key)
    }

    fn cable_resolve(&self, name: &str, kind: DeviceKind, devices: &[DeviceInfo]) -> String {
        vcable::resolve(name, kind, devices)
    }

    fn cable_apply_elevated(&self, actions: &[CableAction]) -> (bool, String) {
        let res = vcable::run_elevated(actions);
        // renamed / disabled endpoints: re-list right away
        self.generation.fetch_add(1, Relaxed);
        res
    }

    fn calibrate(&self, settings: &Settings, log: LogSink) -> Result<(f64, f64)> {
        echoguard_core::calibrate(
            settings,
            self.backend()?,
            CALIBRATE_S,
            CALIBRATE_LEVEL_DB,
            log,
        )
    }

    fn autostart_supported(&self) -> bool {
        cfg!(windows)
    }

    fn autostart_enabled(&self) -> bool {
        winutil::get_autostart().is_some()
    }

    fn set_autostart(&self, enabled: bool) -> Result<()> {
        winutil::set_autostart(enabled)
    }

    fn default_config_path(&self) -> PathBuf {
        default_config_path()
    }

    fn load_settings(&self, path: &Path) -> Result<Settings> {
        load_settings_file(path)
    }

    fn save_settings(&self, path: &Path, settings: &Settings) -> Result<()> {
        save_settings_file(path, settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echoguard_audio::sim::SimBackend;
    use echoguard_dsp::synth_speech;
    use std::time::Instant;

    fn quiet() -> LogSink {
        Arc::new(|_| {})
    }

    fn sim(speed: f64) -> Arc<SimBackend> {
        let speech = synth_speech(4.0, 1);
        Arc::new(
            SimBackend::new(speed)
                .with_input("USB Mic", speech.clone(), 0.0)
                .with_loopback("Speakers (Realtek)", speech, 0.0)
                .with_output("CABLE Input (VB-Audio Virtual Cable)", 0.0)
                .with_output("Speakers (Realtek)", 0.0),
        )
    }

    #[test]
    fn start_status_stop_on_sim_backend() {
        let be = sim(1.0);
        let eng = CoreEngine::with_backend(be.clone(), quiet());
        assert!(!eng.is_running() && eng.status().is_none());
        let st = Settings {
            mic: Some("usb".into()),
            speaker: Some("realtek".into()),
            engine: "nlms".into(),
            stats_interval_s: 0.0,
            ..Default::default()
        };
        eng.start(&st).expect("start");
        assert!(eng.is_running());
        assert_eq!(eng.engine_name().as_deref(), Some("nlms"));
        assert_eq!(eng.guard_latency_ms(), None);
        std::thread::sleep(Duration::from_millis(1200));
        let s = eng.status().expect("status while running");
        assert!(s.error.is_none(), "{:?}", s.error);
        // the level meter left its -120 dB floor: frames are flowing
        assert!(s.mic_db > -119.0, "mic level {}", s.mic_db);
        assert!(eng.take_mic_peak() >= 0.0);
        assert!(!be.output_data("CABLE Input").is_empty());
        // restart with new settings while running (auto-apply path)
        eng.start(&Settings {
            siri_guard: true,
            call_in: "USB".into(),
            ..st.clone()
        })
        .expect("restart");
        assert!(eng.guard_latency_ms().is_some());
        eng.stop();
        assert!(!eng.is_running() && eng.status().is_none() && eng.engine_name().is_none());
        // stop twice is harmless
        eng.stop();
    }

    #[test]
    fn start_error_and_devices() {
        let eng = CoreEngine::with_backend(sim(1.0), quiet());
        let err = eng
            .start(&Settings {
                mic: Some("does-not-exist".into()),
                engine: "nlms".into(),
                ..Default::default()
            })
            .expect_err("unknown mic");
        assert!(format!("{err:#}").contains("does-not-exist"), "{err:#}");
        assert!(!eng.is_running());
        let devs = eng.devices().unwrap();
        assert!(eng.cable_status(&devs).names["out"].is_some());
    }

    #[test]
    fn device_watcher_bumps_generation() {
        let be = sim(1.0);
        let eng = CoreEngine::build(Ok(be.clone()), quiet(), Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(150));
        let g0 = eng.device_generation();
        be.add_output("Headphones (USB)", 0.0);
        let t0 = Instant::now();
        while eng.device_generation() == g0 && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(eng.device_generation() > g0);
        // dropping stops the watcher thread promptly
        let t0 = Instant::now();
        drop(eng);
        assert!(t0.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn no_backend_is_a_plain_error() {
        let eng = CoreEngine::build(Err("無法使用".into()), quiet(), DEVICE_POLL);
        assert!(eng.devices().is_err());
        assert!(eng.start(&Settings::default()).is_err());
        assert!(!eng.is_running());
        assert_eq!(eng.device_generation(), 0);
    }
}
