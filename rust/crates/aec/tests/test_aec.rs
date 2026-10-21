//! Offline AEC verification on synthetic signals (port of tests/test_aec.py).
//!
//! mic = near-end speech + (reference delayed and filtered by a room IR) + noise.
//! Layout: 40 % far-end only, 20 % near-end only, 40 % double talk.
//! Intelligibility uses `envelope_correlation` (what test_aec.py falls back
//! to without pystoi).

use std::sync::OnceLock;

use echoguard_aec::{available_engines, make_engine, AecProcessor, EngineOptions};
use echoguard_dsp::metrics::{align, db, energy, envelope_correlation, erle_db};
use echoguard_dsp::testsig::{make_scenario, Scenario};

fn scenario(delay_ms: f64) -> &'static Scenario {
    static S40: OnceLock<Scenario> = OnceLock::new();
    static S150: OnceLock<Scenario> = OnceLock::new();
    let cell = if delay_ms == 40.0 { &S40 } else { &S150 };
    cell.get_or_init(|| make_scenario(30.0, delay_ms, 3))
}

fn select(x: &[f32], mask: &[bool]) -> Vec<f32> {
    x.iter()
        .zip(mask)
        .filter(|(_, &m)| m)
        .map(|(&v, _)| v)
        .collect()
}

fn intelligibility(clean: &[f32], test: &[f32]) -> f64 {
    let (c, t) = align(clean, test, 2400);
    envelope_correlation(c, t)
}

/// Minimum far-end-only ERLE expected per engine (dB).
fn min_erle(engine: &str) -> f64 {
    match engine {
        "webrtc" => 30.0,
        "speex" => 15.0,
        _ => 10.0,
    }
}

fn run_engine(engine: &str, ns: bool, delay: f64) {
    if !available_engines().contains(&engine) {
        eprintln!("skip: {engine} not built");
        return;
    }
    let sc = scenario(delay);
    let opts = EngineOptions {
        noise_suppression: ns,
        filter_ms: 300,
        ..Default::default()
    };
    let mut proc = AecProcessor::new(make_engine(engine, &opts).unwrap(), 0.0, 0.0, 0.0);
    let out = proc.process_signal(&sc.mic, &sc.reference).unwrap();
    let n = out.len();
    let mic = &sc.mic[..n];
    // what a perfect AEC outputs
    let near_target: Vec<f32> = sc.mic[..n]
        .iter()
        .zip(&sc.echo)
        .map(|(m, e)| m - e)
        .collect();

    let erle = erle_db(mic, &out, Some(&sc.far_only[..n]), 3.0);
    let (nn, dt) = (&sc.near_only[..n], &sc.double_talk[..n]);
    let near_level = db(energy(&select(&out, nn))) - db(energy(&select(&near_target, nn)));
    let near_intel = intelligibility(&select(&near_target, nn), &select(&out, nn));
    let dt_target = select(&near_target, dt);
    let dt_intel_out = intelligibility(&dt_target, &select(&out, dt));
    let dt_intel_mic = intelligibility(&dt_target, &select(mic, dt));
    println!(
        "[metrics] aec[{engine},ns={},delay={delay:.0}ms]: ERLE_far_only_dB={erle:.2}, \
         near_only_level_change_dB={near_level:.2}, near_only_STOI={near_intel:.2}, \
         double_talk_STOI_out={dt_intel_out:.2}, double_talk_STOI_mic={dt_intel_mic:.2}",
        ns as u8
    );

    assert!(erle >= min_erle(engine), "ERLE {erle:.2} dB");
    // near end alone must come through: level within 3 dB, intelligibility kept
    assert!(
        near_level.abs() < 3.0,
        "near level change {near_level:.2} dB"
    );
    assert!(near_intel > 0.85, "near intelligibility {near_intel:.3}");
    // double talk: output must not be worse than doing nothing by a wide margin
    assert!(
        dt_intel_out > dt_intel_mic - 0.15,
        "double talk {dt_intel_out:.3} vs mic {dt_intel_mic:.3}"
    );
}

macro_rules! engine_tests {
    ($($name:ident: $engine:expr, $ns:expr, $delay:expr;)*) => {
        $(#[test] fn $name() { run_engine($engine, $ns, $delay); })*
    };
}

engine_tests! {
    aec_webrtc_ns_off_delay40ms: "webrtc", false, 40.0;
    aec_webrtc_ns_on_delay40ms: "webrtc", true, 40.0;
    aec_webrtc_ns_off_delay150ms: "webrtc", false, 150.0;
    aec_webrtc_ns_on_delay150ms: "webrtc", true, 150.0;
    aec_speex_ns_off_delay40ms: "speex", false, 40.0;
    aec_speex_ns_on_delay40ms: "speex", true, 40.0;
    aec_speex_ns_off_delay150ms: "speex", false, 150.0;
    aec_speex_ns_on_delay150ms: "speex", true, 150.0;
    // nlms has no noise suppressor
    aec_nlms_ns_off_delay40ms: "nlms", false, 40.0;
    aec_nlms_ns_off_delay150ms: "nlms", false, 150.0;
}

#[test]
fn passthrough_has_no_erle() {
    let sc = make_scenario(12.0, 40.0, 0);
    let mut p = AecProcessor::new(
        make_engine("off", &EngineOptions::default()).unwrap(),
        0.0,
        0.0,
        0.0,
    );
    let out = p.process_signal(&sc.mic, &sc.reference).unwrap();
    let e = erle_db(&sc.mic, &out, Some(&sc.far_only[..out.len()]), 0.0);
    assert!(e.abs() < 0.01, "{e}");
}

/// A 600 ms bulk delay is outside a 300 ms nlms filter; --delay-ms fixes it.
#[test]
fn ref_delay_line_helps_short_filter() {
    let sc = make_scenario(20.0, 600.0, 0);
    let opts = EngineOptions {
        filter_ms: 300,
        ..Default::default()
    };
    let plain = AecProcessor::new(make_engine("nlms", &opts).unwrap(), 0.0, 0.0, 0.0)
        .process_signal(&sc.mic, &sc.reference)
        .unwrap();
    let fixed = AecProcessor::new(make_engine("nlms", &opts).unwrap(), 590.0, 0.0, 0.0)
        .process_signal(&sc.mic, &sc.reference)
        .unwrap();
    let e_plain = erle_db(&sc.mic, &plain, Some(&sc.far_only[..plain.len()]), 3.0);
    let e_fixed = erle_db(&sc.mic, &fixed, Some(&sc.far_only[..fixed.len()]), 3.0);
    println!("[metrics] delay_compensation[nlms,600ms]: ERLE_without={e_plain:.2}, ERLE_with_delay_ms={e_fixed:.2}");
    assert!(
        e_plain < 3.0 && e_fixed > 10.0,
        "{e_plain:.2} / {e_fixed:.2}"
    );
}
