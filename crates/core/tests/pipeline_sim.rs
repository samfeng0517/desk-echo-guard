//! End-to-end run of the threaded pipeline against simulated devices
//! (`tests/test_app_sim.py`).
//!
//! Each simulated device runs on its own clock (ppm offsets), like real
//! sound cards, at 2x real time.  This exercises the reader/writer threads,
//! elastic buffers and drift compensation, and the Siri-guard path.

use std::sync::Arc;
use std::time::{Duration, Instant};

use echoguard_aec::available_engines;
use echoguard_audio::sim::SimBackend;
use echoguard_core::{EchoGuard, LogSink, Settings};
use echoguard_dsp::{band_energy_db, best_lag, erle_db, make_scenario, synth_speech};

const SPEED: f64 = 2.0;

/// Band-limited (windowed-sinc) resampling of `x` to `m` samples, standing
/// in for `scipy.signal.resample`.
fn resample_to(x: &[f32], m: usize) -> Vec<f32> {
    const HALF: isize = 16;
    let step = x.len() as f64 / m as f64;
    (0..m)
        .map(|j| {
            let t = j as f64 * step;
            let c = t.floor() as isize;
            let mut acc = 0.0f64;
            for k in (c - HALF + 1)..=(c + HALF) {
                if k < 0 || k as usize >= x.len() {
                    continue;
                }
                let d = t - k as f64;
                let sinc = if d.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d)
                };
                // Hann window over the kernel span
                let w = 0.5 + 0.5 * (std::f64::consts::PI * d / HALF as f64).cos();
                acc += x[k as usize] as f64 * sinc * w;
            }
            acc as f32
        })
        .collect()
}

#[test]
fn threaded_pipeline_with_clock_drift() {
    let webrtc = available_engines().contains(&"webrtc");
    let engine = if webrtc { "webrtc" } else { "nlms" };
    let ppm = if webrtc { 100.0 } else { 0.0 };
    let dur = 16.0;
    let sc = make_scenario(dur, 60.0, 0);
    let n = sc.reference.len();
    // the loopback device has its own clock: same sound, more/less samples
    let lb = resample_to(
        &sc.reference,
        (n as f64 * (1.0 + ppm * 1e-6)).round() as usize,
    );
    let call = synth_speech(dur, 11);
    let be = Arc::new(
        SimBackend::new(SPEED)
            .with_input("USB Mic", sc.mic.clone(), 0.0)
            .with_input("CABLE-A Output", call.clone(), -40.0)
            .with_loopback("Speakers (Realtek)", lb, ppm)
            .with_output("CABLE Input", -60.0)
            .with_output("Speakers (Realtek)", 0.0),
    );
    let st = Settings {
        mic: Some("usb".into()),
        speaker: Some("realtek".into()),
        out: "cable input".into(),
        engine: engine.into(),
        noise_suppression: false,
        stats_interval_s: 0.0,
        siri_guard: true,
        siri_mode: "always".into(),
        call_in: "cable-a".into(),
        // thread-scheduling jitter does not shrink with the speed-up
        ref_buffer_ms: 20.0 * SPEED,
        out_buffer_ms: 40.0 * SPEED,
        spk_buffer_ms: 40.0 * SPEED,
        ..Default::default()
    };
    let log: LogSink = Arc::new(|_| {});
    let eg = EchoGuard::start(&st, be.clone(), log).expect("start");
    let t0 = Instant::now();
    while t0.elapsed().as_secs_f64() < dur / SPEED && !eg.is_stopped() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = eg.status();
    let stats = eg.stats_line();
    eg.stop().expect("pipeline error");

    let out = be.output_data("CABLE Input");
    let lag = best_lag(&sc.near, &out[..n.min(out.len())], 48_000);
    let o = &out[lag.max(0) as usize..];
    let m = o.len().min(n);
    let erle = erle_db(&sc.mic[..m], &o[..m], Some(&sc.far_only[..m]), 3.0);
    let spk = be.output_data("Speakers (Realtek)");
    let k = spk.len().min(call.len());
    let drop = band_energy_db(&call[..k], 3800.0, 4200.0) - band_energy_db(&spk, 3800.0, 4200.0);
    println!(
        "threaded_sim[{engine},loopback {ppm:+.0} ppm] ERLE_far_only_dB={erle:.1} \
         pipeline_latency_ms={:.1} ref_drift_estimate_ppm={:.1} ref_underruns={} \
         guard_4k_band_drop_dB={drop:.1}\n  {stats}",
        lag as f64 / 48.0,
        status.drift_ppm,
        status.ref_underruns
    );
    assert!(erle > if webrtc { 25.0 } else { 8.0 }, "ERLE {erle:.1} dB");
    let lat = lag as f64 / 48.0;
    assert!(0.0 < lat && lat < 300.0, "latency {lat} ms");
    assert!(drop > 15.0, "guard band drop {drop:.1} dB");
}

/// Voice typing: while the watched program records, the virtual mic is
/// silent; before and after it carries the mic.
#[test]
fn dictation_mutes_the_virtual_mic() {
    use echoguard_core::dictation::{DictationWatcher, WatcherFactory};

    /// "Records" from 1 s to 2 s after it was opened (2 s .. 4 s of audio).
    struct Fake(Instant);
    impl DictationWatcher for Fake {
        fn recording(&mut self) -> anyhow::Result<Option<String>> {
            let t = self.0.elapsed().as_secs_f64();
            Ok((1.0..2.0).contains(&t).then(|| "Typeless.exe".to_string()))
        }
    }

    let dur = 6.0;
    let mic = synth_speech(dur, 3);
    let be = Arc::new(
        SimBackend::new(SPEED)
            .with_input("USB Mic", mic, 0.0)
            .with_loopback(
                "Speakers (Realtek)",
                vec![0.0; (dur * 48_000.0) as usize],
                0.0,
            )
            .with_output("CABLE Input", 0.0),
    );
    let st = Settings {
        mic: Some("usb".into()),
        speaker: Some("realtek".into()),
        out: "cable input".into(),
        engine: "off".into(),
        stats_interval_s: 0.0,
        ref_buffer_ms: 20.0 * SPEED,
        out_buffer_ms: 40.0 * SPEED,
        ..Default::default()
    };
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = lines.clone();
    let log: LogSink = Arc::new(move |l| sink.lock().unwrap().push(l));
    let t0 = Instant::now();
    let open: WatcherFactory =
        Box::new(move || Ok(Box::new(Fake(t0)) as Box<dyn DictationWatcher>));
    let eg = EchoGuard::start_with(&st, be.clone(), log, Some(open)).expect("start");
    let mut seen = Vec::new();
    while t0.elapsed().as_secs_f64() < dur / SPEED && !eg.is_stopped() {
        std::thread::sleep(Duration::from_millis(20));
        seen.push((t0.elapsed().as_secs_f64(), eg.status().dictation_muted));
    }
    eg.stop().expect("pipeline error");

    // the status follows the program (plus the short hold)
    let at = |a: f64, b: f64| seen.iter().filter(move |(t, _)| (a..b).contains(t));
    assert!(at(0.0, 0.9).all(|(_, m)| !m), "{seen:?}");
    assert!(at(1.2, 1.9).all(|(_, m)| *m), "{seen:?}");
    assert!(at(2.5, 3.0).all(|(_, m)| !m), "{seen:?}");

    let out = be.output_data("CABLE Input");
    let level = |a: f64, b: f64| {
        let (a, b) = (
            (a * 48_000.0) as usize,
            ((b * 48_000.0) as usize).min(out.len()),
        );
        echoguard_dsp::db(echoguard_dsp::energy(&out[a..b]))
    };
    let (before, during, after) = (level(0.5, 1.5), level(2.6, 3.8), level(4.8, 5.6));
    println!("dictation_sim before={before:.1} dB during={during:.1} dB after={after:.1} dB");
    assert!(
        before > -40.0 && after > -40.0,
        "{before:.1} / {after:.1} dB"
    );
    assert!(during < -100.0, "{during:.1} dB while dictating");
    let lines = lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("Typeless.exe is recording")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("hears the mic again")),
        "{lines:?}"
    );
}
