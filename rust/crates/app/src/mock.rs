//! A stand-in [`Engine`] with fake devices (incl. VB-Cable) and animated
//! levels, for `--screenshot` and UI work (`DEG_MOCK=1`).  Settings really
//! are loaded from / saved to the TOML file; VB-Cable role logic is core's.
//!
//! Environment knobs (for trying out InfoBars):
//!   DEG_MOCK_NO_CABLE=1  no VB-Cable installed
//!   DEG_MOCK_NO_AB=1     no VB-CABLE A+B (Siri guard requirement)
//!   DEG_MOCK_LOUD=1      mic clips now and then
//!   DEG_MOCK_FAIL=1      the pipeline "fails" ~8 s after start

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use parking_lot::Mutex;

use echoguard_core::vcable::{self, role_of};

use crate::engine::*;

#[derive(Clone, Debug)]
pub struct MockOptions {
    pub cable: bool,
    pub cable_ab: bool,
    pub renamed: bool,
    pub loud_mic: bool,
    pub fail_after: Option<Duration>,
    /// Make start/stop/calibrate take realistic time.
    pub delays: bool,
}

impl Default for MockOptions {
    fn default() -> Self {
        MockOptions {
            cable: true,
            cable_ab: true,
            renamed: false,
            loud_mic: false,
            fail_after: None,
            delays: true,
        }
    }
}

impl MockOptions {
    pub fn from_env() -> Self {
        let on = |k: &str| std::env::var(k).is_ok_and(|v| v != "0" && !v.is_empty());
        MockOptions {
            cable: !on("DEG_MOCK_NO_CABLE"),
            cable_ab: !on("DEG_MOCK_NO_AB"),
            loud_mic: on("DEG_MOCK_LOUD"),
            fail_after: on("DEG_MOCK_FAIL").then_some(Duration::from_secs(8)),
            ..Default::default()
        }
    }
}

struct Run {
    since: Instant,
    engine: String,
    siri: bool,
}

pub struct MockEngine {
    opts: MockOptions,
    log: LogSink,
    run: Mutex<Option<Run>>,
    renamed: AtomicBool,
    hidden: AtomicBool,
    autostart: AtomicBool,
    generation: AtomicU64,
    peak_seq: AtomicU64,
}

impl MockEngine {
    pub fn new(opts: MockOptions, log: LogSink) -> Self {
        let renamed = opts.renamed;
        MockEngine {
            opts,
            log,
            run: Mutex::new(None),
            renamed: AtomicBool::new(renamed),
            hidden: AtomicBool::new(false),
            autostart: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            peak_seq: AtomicU64::new(0),
        }
    }

    fn sleep(&self, ms: u64) {
        if self.opts.delays {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

fn dev(kind: DeviceKind, name: &str, default: bool) -> DeviceInfo {
    DeviceInfo {
        kind,
        name: name.to_string(),
        id: format!("mock:{name}"),
        channels: 2,
        default,
    }
}

const VB: &str = " (VB-Audio Virtual Cable)";
const VBA: &str = " (VB-Audio Cable A)";
const VBB: &str = " (VB-Audio Cable B)";

impl Engine for MockEngine {
    fn start(&self, s: &Settings) -> Result<()> {
        self.sleep(600);
        if s.siri_guard && s.siri_mode == "detect" && s.siri_model.is_empty() {
            bail!("detect mode needs --siri-model");
        }
        if !self.opts.cable {
            bail!("no speaker matches \"CABLE Input\". Available:\n  喇叭 (Realtek(R) Audio)");
        }
        let engine = if s.engine == "auto" {
            "webrtc".to_string()
        } else {
            s.engine.clone()
        };
        (self.log)(format!(
            "mic={} speaker={} out={} engine={engine}",
            s.mic.as_deref().unwrap_or("(default)"),
            s.speaker.as_deref().unwrap_or("(default)"),
            s.out
        ));
        *self.run.lock() = Some(Run {
            since: Instant::now(),
            engine,
            siri: s.siri_guard,
        });
        Ok(())
    }

    fn stop(&self) {
        if self.run.lock().take().is_some() {
            self.sleep(300);
        }
    }

    fn is_running(&self) -> bool {
        self.run.lock().is_some()
    }

    fn status(&self) -> Option<Status> {
        let run = self.run.lock();
        let run = run.as_ref()?;
        let t = run.since.elapsed().as_secs_f32();
        // speech-like envelopes: syllables (~4 Hz) gated by phrases
        let env = |phase: f32, rate: f32| {
            let syll = (t * rate * std::f32::consts::TAU + phase).sin().abs();
            let phrase = ((t * 0.31 + phase).sin() * 0.5 + 0.5).powf(0.6);
            syll * phrase
        };
        let mic = -58.0 + 40.0 * env(0.0, 3.7);
        let refl = -55.0 + 42.0 * env(1.7, 4.3);
        let out = mic - 6.0 + 4.0 * (t * 9.0).sin();
        let error = self
            .opts
            .fail_after
            .filter(|d| run.since.elapsed() > *d)
            .map(|_| "WASAPI: device invalidated (AUDCLNT_E_DEVICE_INVALIDATED)".to_string());
        Some(Status {
            mic_db: mic,
            ref_db: refl,
            out_db: out,
            ref_buf_ms: 20.0 + 1.5 * (t * 0.7).sin(),
            drift_ppm: 42.0 + 3.0 * (t * 0.05).sin(),
            ref_underruns: 0,
            out_underruns: (t / 120.0) as u64,
            aec_ms: 0.42 + 0.05 * (t * 1.3).sin(),
            siri_detections: if run.siri { (t / 9.0) as u64 } else { 0 },
            mic_peak: 0.3,
            error,
        })
    }

    fn take_mic_peak(&self) -> f32 {
        if !self.opts.loud_mic || !self.is_running() {
            return 0.2;
        }
        // every ~20th poll reports clipping
        if self
            .peak_seq
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(20)
        {
            0.97
        } else {
            0.5
        }
    }

    fn engine_name(&self) -> Option<String> {
        self.run.lock().as_ref().map(|r| r.engine.clone())
    }

    fn guard_latency_ms(&self) -> Option<f64> {
        self.run.lock().as_ref().filter(|r| r.siri).map(|_| 12.5)
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>> {
        use DeviceKind::{Mic, Speaker};
        let renamed = self.renamed.load(Ordering::Relaxed);
        let hidden = self.hidden.load(Ordering::Relaxed);
        let name = |orig: &str, suffix: &str| {
            let r = role_of(orig, None).filter(|_| renamed);
            format!("{}{}", r.map_or(orig, |r| r.renamed), suffix)
        };
        let mut v = vec![
            dev(Mic, "麥克風 (Realtek(R) Audio)", true),
            dev(Mic, "耳機麥克風 (USB Audio Device)", false),
            dev(Speaker, "喇叭 (Realtek(R) Audio)", true),
            dev(Speaker, "DELL U2720Q (NVIDIA High Definition Audio)", false),
        ];
        if self.opts.cable {
            v.push(dev(Mic, &name("CABLE Output", VB), false));
            v.push(dev(Speaker, &name("CABLE Input", VB), false));
            if !hidden {
                v.push(dev(Speaker, &format!("CABLE In 16ch{VB}"), false));
            }
        }
        if self.opts.cable_ab {
            v.push(dev(Mic, &name("CABLE-A Output", VBA), false));
            v.push(dev(Speaker, &name("CABLE-A Input", VBA), false));
            if !hidden {
                v.push(dev(Mic, &format!("CABLE-B Output{VBB}"), false));
                v.push(dev(Speaker, &format!("CABLE-B Input{VBB}"), false));
            }
        }
        Ok(v)
    }

    fn device_generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
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
        self.sleep(1500);
        let mut done = Vec::new();
        for a in actions {
            match a {
                CableAction::Rename => {
                    self.renamed.store(true, Ordering::Relaxed);
                    done.push("renamed 4 endpoints");
                }
                CableAction::Hide => {
                    self.hidden.store(true, Ordering::Relaxed);
                    done.push("disabled 3 endpoints");
                }
                CableAction::Restore => {
                    self.renamed.store(false, Ordering::Relaxed);
                    self.hidden.store(false, Ordering::Relaxed);
                    done.push("restored names, re-enabled endpoints");
                }
            }
        }
        self.generation.fetch_add(1, Ordering::Relaxed);
        (true, done.join("; "))
    }

    fn calibrate(&self, s: &Settings, log: LogSink) -> Result<(f64, f64)> {
        log("calibrating: playing 4 s of noise".into());
        self.sleep(4000);
        let d = if s.engine == "speex" { 284.0 } else { 37.5 };
        log(format!("estimated delay {d:.1} ms (confidence 11.2)"));
        Ok((d, 11.2))
    }

    fn autostart_supported(&self) -> bool {
        cfg!(windows)
    }

    fn autostart_enabled(&self) -> bool {
        self.autostart.load(Ordering::Relaxed)
    }

    fn set_autostart(&self, enabled: bool) -> Result<()> {
        self.autostart.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    fn default_config_path(&self) -> PathBuf {
        default_config_path()
    }

    fn load_settings(&self, path: &Path) -> Result<Settings> {
        load_settings_file(path)
    }

    fn save_settings(&self, path: &Path, s: &Settings) -> Result<()> {
        save_settings_file(path, s)
    }
}
