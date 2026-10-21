//! Pipeline against simulated devices: renamed / hot-plugged VB-Cable
//! (`tests/test_vcable.py`, `tests/test_gui.py`), error propagation,
//! debug wavs and calibration.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use echoguard_audio::sim::SimBackend;
use echoguard_audio::{AudioBackend, DeviceInfo, InputStream, OutputStream};
use echoguard_core::pipeline::calibrate;
use echoguard_core::{run_blocking, vcable, EchoGuard, LogSink, Settings};
use echoguard_dsp::{make_scenario, synth_speech};
use parking_lot::Mutex;

fn quiet() -> LogSink {
    Arc::new(|_| {})
}

fn collecting() -> (LogSink, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let l = lines.clone();
    (Arc::new(move |s| l.lock().push(s)), lines)
}

fn sleep_s(s: f64) {
    std::thread::sleep(Duration::from_secs_f64(s));
}

#[test]
fn app_finds_renamed_cable_with_default_settings() {
    let sc = make_scenario(3.0, 40.0, 0);
    let sink = "EchoGuard 內部輸出（勿選） (VB-Audio Virtual Cable)";
    let be = Arc::new(
        SimBackend::new(4.0)
            .with_input("USB Mic", sc.mic.clone(), 0.0)
            .with_loopback("Speakers", sc.reference.clone(), 0.0)
            .with_output(sink, 0.0)
            .with_output("Speakers", 0.0),
    );
    let s = Settings {
        engine: "nlms".into(),
        stats_interval_s: 0.0,
        ..Default::default()
    };
    run_blocking(&s, be.clone(), Some(0.5), quiet()).unwrap();
    let out = be.output_data(sink);
    assert!(!out.is_empty());
    assert!(out.iter().all(|v| v.is_finite()));
}

#[test]
fn hot_plugged_cable_is_found_and_used() {
    let be = Arc::new(
        SimBackend::new(4.0)
            .with_input("USB Mic", vec![0.0f32; 480], 0.0)
            .with_loopback("Speakers", vec![0.0f32; 480], 0.0)
            .with_output("Speakers", 0.0),
    );
    let s = Settings {
        engine: "nlms".into(),
        mic: Some("USB Mic".into()),
        stats_interval_s: 0.0,
        ..Default::default()
    };
    let devs = be.list_devices().unwrap();
    assert!(!vcable::status(&devs).found);
    // no "CABLE Input" yet: start fails cleanly
    assert!(EchoGuard::start(&s, be.clone(), quiet()).is_err());

    be.add_input(
        "CABLE Output (VB-Audio Virtual Cable)",
        vec![0.0f32; 480],
        0.0,
        false,
    );
    be.add_output("CABLE Input (VB-Audio Virtual Cable)", 0.0);
    be.add_output("CABLE-A Input (VB-Audio Cable A)", 0.0);
    let devs = be.list_devices().unwrap();
    let st = vcable::status(&devs);
    assert!(st.found && !st.renamed);
    assert_eq!(vcable::display_name(&devs, "virtual_mic"), "CABLE Output");

    let eg = EchoGuard::start(&s, be.clone(), quiet()).unwrap();
    sleep_s(0.3);
    eg.stop().unwrap();
    assert!(!be
        .output_data("CABLE Input (VB-Audio Virtual Cable)")
        .is_empty());

    be.add_output("CABLE In 16ch (VB-Audio Virtual Cable)", 0.0);
    let st = vcable::status(&be.list_devices().unwrap());
    assert_eq!(st.unused, vec!["CABLE In 16ch (VB-Audio Virtual Cable)"]);
}

#[test]
fn renamed_call_input_is_resolved_for_the_guard() {
    let call_in = "EchoGuard 內部輸入（勿選） (VB-Audio Cable A)";
    let be = Arc::new(
        SimBackend::new(4.0)
            .with_input("USB Mic", synth_speech(2.0, 1), 0.0)
            .with_input(call_in, synth_speech(2.0, 2), 0.0)
            .with_loopback("Speakers", vec![0.0f32; 480], 0.0)
            .with_output("CABLE Input", 0.0)
            .with_output("Speakers", 0.0),
    );
    let s = Settings {
        engine: "nlms".into(),
        mic: Some("USB Mic".into()),
        speaker: Some("Speakers".into()),
        siri_guard: true,
        // default call_in "CABLE-A Output" must follow the rename
        ..Default::default()
    };
    let eg = EchoGuard::start(&s, be.clone(), quiet()).unwrap();
    assert!(eg.guard_latency_ms().unwrap() < 50.0);
    sleep_s(0.3);
    eg.stop().unwrap();
    assert!(!be.output_data("Speakers").is_empty());
}

/// GUI-style session (`test_gui_start_stop_and_config_roundtrip`, pipeline
/// part): meters move, guard path runs, stop leaves the outputs written.
#[test]
fn start_status_stop_with_guard() {
    let sc = make_scenario(10.0, 40.0, 0);
    let be = Arc::new(
        SimBackend::new(1.0)
            .with_input("USB Mic", sc.mic.clone(), 0.0)
            .with_input("CABLE-A Output", synth_speech(10.0, 0), 0.0)
            .with_loopback("Speakers", sc.reference.clone(), 0.0)
            .with_output("CABLE Input", 0.0)
            .with_output("Speakers", 0.0),
    );
    let s = Settings {
        mic: Some("USB Mic".into()),
        speaker: Some("Speakers".into()),
        out: "CABLE Input".into(),
        engine: "nlms".into(),
        siri_guard: true,
        call_in: "CABLE-A Output".into(),
        siri_mode: "always".into(),
        ..Default::default()
    };
    let (log, lines) = collecting();
    let eg = EchoGuard::start(&s, be.clone(), log).unwrap();
    assert_eq!(eg.engine_name(), "nlms");
    sleep_s(1.5);
    assert!(eg.frames() > 50, "frames {}", eg.frames());
    let st = eg.status();
    assert!(st.mic_db > -120.0 && st.mic_db < 0.0, "{st:?}");
    assert!(st.error.is_none() && !eg.is_failed());
    assert!(eg.take_mic_peak() > 0.0);
    let line = eg.stats_line();
    assert!(
        line.starts_with("mic ") && line.contains("siri detections="),
        "{line}"
    );
    eg.stop().unwrap();
    assert!(!be.output_data("CABLE Input").is_empty());
    assert!(!be.output_data("Speakers").is_empty());
    let lines = lines.lock();
    assert!(lines[0].starts_with("[aec] engine=nlms"), "{lines:?}");
    assert!(
        lines[1].starts_with("[siri-guard] mode=always"),
        "{lines:?}"
    );
}

#[test]
fn debug_wavs_are_written() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("debug-wav-test");
    let _ = std::fs::remove_dir_all(&dir);
    let be = Arc::new(
        SimBackend::new(4.0)
            .with_input("Mic", synth_speech(2.0, 3), 0.0)
            .with_input("CABLE-A Output", synth_speech(2.0, 4), 0.0)
            .with_loopback("Speakers", synth_speech(2.0, 5), 0.0)
            .with_output("CABLE Input", 0.0)
            .with_output("Speakers", 0.0),
    );
    let s = Settings {
        engine: "off".into(),
        siri_guard: true,
        debug_wav: Some(dir.to_string_lossy().into_owned()),
        debug_max_s: 0.5,
        stats_interval_s: 0.0,
        ..Default::default()
    };
    run_blocking(&s, be, Some(0.6), quiet()).unwrap();
    for k in ["mic", "ref", "out", "call"] {
        let r = hound::WavReader::open(dir.join(format!("{k}.wav"))).unwrap();
        assert_eq!(r.spec().sample_rate, 48_000);
        // bounded: 0.5 s plus at most one block
        assert!(r.len() >= 24_000 && r.len() <= 24_480, "{k}: {}", r.len());
    }
}

// ------------------------------------------------------- error propagation

struct FailingInput {
    reads: Arc<AtomicUsize>,
    fail_after: usize,
}

impl InputStream for FailingInput {
    fn read(&mut self, out: &mut [f32]) -> Result<()> {
        std::thread::sleep(Duration::from_millis(2));
        if self.reads.fetch_add(1, Ordering::Relaxed) >= self.fail_after {
            bail!("device unplugged");
        }
        out.fill(0.0);
        Ok(())
    }
}

struct NullOutput;

impl OutputStream for NullOutput {
    fn write(&mut self, _x: &[f32]) -> Result<()> {
        std::thread::sleep(Duration::from_millis(2));
        Ok(())
    }
}

struct FailingBackend;

impl AudioBackend for FailingBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        bail!("no device list")
    }
    fn open_input(
        &self,
        _name: Option<&str>,
        loopback: bool,
        _bs: usize,
    ) -> Result<Box<dyn InputStream>> {
        Ok(Box::new(FailingInput {
            reads: Arc::new(AtomicUsize::new(0)),
            fail_after: if loopback { usize::MAX } else { 20 },
        }))
    }
    fn open_output(&self, _name: Option<&str>, _bs: usize) -> Result<Box<dyn OutputStream>> {
        Ok(Box::new(NullOutput))
    }
}

#[test]
fn first_thread_error_stops_everything() {
    let s = Settings {
        engine: "off".into(),
        ..Default::default()
    };
    let eg = EchoGuard::start(&s, Arc::new(FailingBackend), quiet()).unwrap();
    let t0 = Instant::now();
    while !eg.is_failed() && t0.elapsed() < Duration::from_secs(5) {
        sleep_s(0.01);
    }
    assert!(eg.is_failed() && eg.is_stopped());
    let err = eg.status().error.unwrap();
    assert!(
        err.contains("device unplugged") && err.contains("aec"),
        "{err}"
    );
    let t0 = Instant::now();
    let e = eg.stop().unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert!(format!("{e:#}").contains("device unplugged"));

    // run_blocking returns the error too
    let e = run_blocking(&s, Arc::new(FailingBackend), Some(5.0), quiet()).unwrap_err();
    assert!(format!("{e:#}").contains("device unplugged"));
}

// ------------------------------------------------------------ calibration

#[test]
fn calibrate_runs_and_warns_on_silent_loopback() {
    let x = synth_speech(4.0, 7);
    let be = Arc::new(
        SimBackend::new(4.0)
            .with_input("Mic", x.clone(), 0.0)
            .with_loopback("Speakers", x, 0.0)
            .with_output("Speakers", 0.0),
    );
    let (log, lines) = collecting();
    let (d, conf) = calibrate(&Settings::default(), be.clone(), 1.0, -20.0, log).unwrap();
    assert!(
        d.is_finite() && d.abs() <= 500.0 && conf.is_finite(),
        "{d} {conf}"
    );
    assert!(lines.lock().iter().any(|l| l.contains("ref->mic delay")));
    assert!(!lines.lock().iter().any(|l| l.contains("silent")));
    // the noise bursts went to the speakers
    assert_eq!(be.output_data("Speakers").len(), 48_000);

    let be = Arc::new(
        SimBackend::new(4.0)
            .with_input("Mic", vec![0.0f32; 480], 0.0)
            .with_loopback("Speakers", vec![0.0f32; 480], 0.0)
            .with_output("Speakers", 0.0),
    );
    let (log, lines) = collecting();
    calibrate(&Settings::default(), be, 0.5, -20.0, log).unwrap();
    assert!(lines
        .lock()
        .iter()
        .any(|l| l.contains("loopback stayed silent")));
}

#[test]
fn start_reports_device_errors() {
    let be = Arc::new(SimBackend::new(4.0).with_output("Speakers", 0.0));
    let s = Settings {
        engine: "off".into(),
        ..Default::default()
    };
    assert!(EchoGuard::start(&s, be, quiet()).is_err());
    let s = Settings {
        engine: "bogus".into(),
        ..Default::default()
    };
    assert!(EchoGuard::start(&s, Arc::new(SimBackend::new(4.0)), quiet()).is_err());
}
