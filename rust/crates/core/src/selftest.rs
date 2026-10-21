//! Quick self-check of the installed program (`selftest.py`; no audio
//! devices needed): which AEC engines are built in, offline ERLE on a
//! synthetic echo, the Siri notch, and whether the audio API answers.
//!
//! ```text
//! DeskEchoGuard.exe --self-test [report.txt]
//! ```

use std::panic::{self, AssertUnwindSafe};
use std::path::Path;

use anyhow::Result;
use echoguard_aec::{available_engines, make_engine, AecProcessor, EngineOptions};
use echoguard_audio::DeviceKind;
use echoguard_dsp::{band_energy_db, erle_db, make_scenario, synth_speech, FRAME};

use crate::pipeline::make_guard;
use crate::settings::{config_text, parse_config, settings_from_table, Settings};

fn mark(good: bool) -> &'static str {
    if good {
        "ok  "
    } else {
        "FAIL"
    }
}

/// Engines, offline ERLE per engine and the Siri notch; appends lines and
/// returns whether everything passed.
fn dsp_checks(lines: &mut Vec<String>) -> Result<bool> {
    let mut ok = true;
    let engines = available_engines();
    lines.push(format!("engines: {}", engines.join(", ")));
    for need in ["webrtc", "speex"] {
        if !engines.contains(&need) {
            ok = false;
            lines.push(format!("FAIL: engine {need} not available"));
        }
    }
    let sc = make_scenario(12.0, 40.0, 0);
    let floors = [("webrtc", 25.0), ("speex", 10.0), ("nlms", 5.0)];
    for (eng, floor) in floors.iter().filter(|(e, _)| engines.contains(e)) {
        let opts = EngineOptions {
            noise_suppression: false,
            ..Default::default()
        };
        let out = AecProcessor::new(make_engine(eng, &opts)?, 0.0, 0.0, 0.0)
            .process_signal(&sc.mic, &sc.reference)?;
        let e = erle_db(&sc.mic, &out, Some(&sc.far_only[..out.len()]), 2.0);
        let good = e >= *floor;
        ok &= good;
        lines.push(format!("{} AEC {eng}: ERLE {e:.1} dB", mark(good)));
    }
    let x = synth_speech(3.0, 0);
    let guard_settings = Settings {
        siri_mode: "always".into(),
        ..Default::default()
    };
    let y = make_guard(&guard_settings)?.process_signal(&x, FRAME);
    let drop = band_energy_db(&x, 3800.0, 4200.0) - band_energy_db(&y, 3800.0, 4200.0);
    let good = drop > 15.0;
    ok &= good;
    lines.push(format!("{} siri notch 4 kHz: -{drop:.1} dB", mark(good)));
    Ok(ok)
}

/// Replaces the Python "window icon bundled" check (the icon is embedded in
/// the exe): default settings survive a save / load round trip.
fn settings_check() -> Result<bool> {
    let s = Settings::default();
    let back = settings_from_table(&parse_config(&config_text(&s))?, true)?;
    Ok(back == s)
}

/// Wake-word (`detect` mode) support: the ONNX Runtime is linked in; the
/// openWakeWord feature models are downloaded on first use.
fn wakeword_line() -> String {
    let cached = echoguard_wakeword::cache_dir().filter(|d| {
        d.join(echoguard_wakeword::MELSPEC_FILE).is_file()
            && d.join(echoguard_wakeword::EMBEDDING_FILE).is_file()
    });
    match cached {
        Some(d) => format!(
            "ok   wake-word runtime built in, feature models in {} (detect mode)",
            d.display()
        ),
        None => "info wake-word runtime built in; feature models are downloaded on first use \
                 (detect mode)"
            .into(),
    }
}

fn device_lines(lines: &mut Vec<String>) {
    // informational only: CI machines have no sound devices
    match echoguard_audio::default_backend().and_then(|b| b.list_devices()) {
        Ok(devs) => {
            lines.push(format!("info audio devices: {}", devs.len()));
            for d in devs {
                let kind = match d.kind {
                    DeviceKind::Mic => "mic",
                    DeviceKind::Speaker => "speaker",
                };
                lines.push(format!("       {kind:7} {}", d.name));
            }
        }
        Err(e) => lines.push(format!("info audio API not usable here: {e:#}")),
    }
}

/// Run every check: `(passed, report lines)`.
pub fn run() -> (bool, Vec<String>) {
    let mut lines = vec![
        format!(
            "desk-echo-guard {} on {} {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        format!(
            "build: {}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        ),
    ];
    let mut ok = true;
    match panic::catch_unwind(AssertUnwindSafe(|| dsp_checks(&mut lines))) {
        Ok(Ok(good)) => ok &= good,
        Ok(Err(e)) => {
            ok = false;
            lines.push(format!("FAIL: {e:?}"));
        }
        Err(_) => {
            ok = false;
            lines.push("FAIL: panic in the DSP checks".into());
        }
    }
    let good = settings_check().unwrap_or(false);
    ok &= good;
    lines.push(format!("{} settings file round trip", mark(good)));
    lines.push(wakeword_line());
    device_lines(&mut lines);
    lines.push(format!("RESULT: {}", if ok { "PASS" } else { "FAIL" }));
    (ok, lines)
}

/// `--self-test [report]`: print (and optionally write) the report;
/// exit code 0 = pass.
pub fn main(report_path: Option<&Path>) -> i32 {
    let (ok, lines) = run();
    let text = lines.join("\n") + "\n";
    if let Some(p) = report_path {
        if let Err(e) = std::fs::write(p, &text) {
            eprintln!("cannot write {}: {e}", p.display());
        }
    }
    print!("{text}");
    if ok {
        0
    } else {
        1
    }
}
