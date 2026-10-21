//! Start / stop the pipeline many times: no threads left behind and memory
//! stays flat.  Alone in its own test binary so other tests' threads do not
//! disturb the counts (Linux: /proc/self).

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::Duration;

use echoguard_aec::available_engines;
use echoguard_audio::sim::SimBackend;
use echoguard_core::{EchoGuard, LogSink, Settings};
use echoguard_dsp::synth_speech;

fn live_threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap()
}

#[test]
fn start_stop_20_times_leaks_nothing() {
    let x = synth_speech(2.0, 1);
    let be = SimBackend::new(4.0);
    be.add_input("USB Mic", x.clone(), 0.0, true);
    be.add_input("CABLE-A Output", x.clone(), 0.0, true);
    be.add_loopback("Speakers", x, 0.0, true);
    be.add_output("CABLE Input", 0.0);
    be.add_output("Speakers", 0.0);
    let be = Arc::new(be);
    let engine = if available_engines().contains(&"webrtc") {
        "webrtc"
    } else {
        "nlms"
    };
    let s = Settings {
        mic: Some("USB Mic".into()),
        speaker: Some("Speakers".into()),
        engine: engine.into(),
        siri_guard: true,
        siri_mode: "always".into(),
        stats_interval_s: 0.0,
        ..Default::default()
    };
    let log: LogSink = Arc::new(|_| {});
    let threads0 = live_threads();
    let cycle = || {
        let eg = EchoGuard::start(&s, be.clone(), log.clone()).unwrap();
        assert_eq!(live_threads(), threads0 + 5);
        std::thread::sleep(Duration::from_millis(150));
        assert!(eg.frames() > 0);
        eg.stop().unwrap();
    };
    // warm-up: lazily created runtime state (allocator arenas, engines)
    for _ in 0..3 {
        cycle();
    }
    let rss0 = rss_kb();
    // LEAK_CYCLES=n runs more cycles (to see whether growth plateaus)
    let n: usize = std::env::var("LEAK_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    for i in 0..n {
        cycle();
        if i % 20 == 19 {
            println!("after {} cycles: {} kB", i + 1, rss_kb());
        }
    }
    let rss1 = rss_kb();
    assert_eq!(live_threads(), threads0, "threads left behind");
    println!("RSS after warm-up {rss0} kB, after {n} more cycles {rss1} kB");
    assert!(
        rss1 < rss0 + 4 * 1024,
        "memory grew from {rss0} kB to {rss1} kB"
    );
}
