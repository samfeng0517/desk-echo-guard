//! CPU time per 10 ms frame and resident memory of the whole pipeline on
//! simulated devices (Linux: reads /proc/self).
//!
//! ```text
//! cargo run --release -p echoguard-core --example bench_pipeline [rss_seconds] [--rss-only]
//! ```
//!
//! Uses looping inputs and outputs that discard their data, so the numbers
//! are not inflated by test recordings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use echoguard_audio::sim::SimInput;
use echoguard_audio::{AudioBackend, DeviceInfo, DeviceKind, InputStream, OutputStream};
use echoguard_core::{EchoGuard, LogSink, Settings};
use echoguard_dsp::{make_scenario, synth_speech, FS};

/// Paced output that drops the samples.
struct NullOutput {
    rate: f64,
    t0: Option<Instant>,
    count: u64,
}

impl OutputStream for NullOutput {
    fn write(&mut self, x: &[f32]) -> Result<()> {
        let t0 = *self.t0.get_or_insert_with(Instant::now);
        self.count += x.len() as u64;
        let due = t0 + Duration::from_secs_f64(self.count as f64 / self.rate);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        Ok(())
    }
}

struct BenchBackend {
    speed: f64,
    mic: Arc<[f32]>,
    reference: Arc<[f32]>,
    call: Arc<[f32]>,
}

impl AudioBackend for BenchBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let dev = |kind, name: &str| DeviceInfo {
            kind,
            name: name.into(),
            id: name.into(),
            channels: 2,
            default: false,
        };
        Ok(vec![
            dev(DeviceKind::Mic, "Mic"),
            dev(DeviceKind::Mic, "CABLE-A Output"),
            dev(DeviceKind::Speaker, "Speakers"),
            dev(DeviceKind::Speaker, "CABLE Input"),
        ])
    }

    fn open_input(
        &self,
        name: Option<&str>,
        loopback: bool,
        _blocksize: usize,
    ) -> Result<Box<dyn InputStream>> {
        let sig = if loopback {
            &self.reference
        } else if name.is_some_and(|n| n.contains("CABLE")) {
            &self.call
        } else {
            &self.mic
        };
        Ok(Box::new(SimInput::new(sig.clone(), 0.0, self.speed, true)))
    }

    fn open_output(&self, _name: Option<&str>, _blocksize: usize) -> Result<Box<dyn OutputStream>> {
        Ok(Box::new(NullOutput {
            rate: FS as f64 * self.speed,
            t0: None,
            count: 0,
        }))
    }

    fn speed(&self) -> f64 {
        self.speed
    }
}

/// Process CPU time (user + system, all threads) in seconds.
fn cpu_s() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // fields after the ")" of the command name; utime / stime are 14 / 15
    let rest = s.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / 100.0 // CLK_TCK = 100 on Linux
}

fn status_kb(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn settings(guard: bool) -> Settings {
    Settings {
        mic: Some("Mic".into()),
        speaker: Some("Speakers".into()),
        engine: "webrtc".into(),
        siri_guard: guard,
        siri_mode: "always".into(),
        stats_interval_s: 0.0,
        ..Default::default()
    }
}

fn backend(speed: f64) -> Arc<BenchBackend> {
    let sc = make_scenario(8.0, 40.0, 0);
    Arc::new(BenchBackend {
        speed,
        mic: sc.mic.into(),
        reference: sc.reference.into(),
        call: synth_speech(8.0, 3).into(),
    })
}

fn main() -> Result<()> {
    let rss_only = std::env::args().any(|a| a == "--rss-only");
    let rss_seconds: f64 = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(20.0);
    let log: LogSink = Arc::new(|_| {});
    println!("RSS at start: {} kB", status_kb("VmRSS:"));

    // CPU per frame: 10x real time for 8 s of wall time per configuration
    let speed = 10.0;
    let configs: &[bool] = if rss_only { &[] } else { &[false, true] };
    for &guard in configs {
        let be = backend(speed);
        let c0 = cpu_s();
        let t0 = Instant::now();
        let eg = EchoGuard::start(&settings(guard), be, log.clone())?;
        std::thread::sleep(Duration::from_secs(8));
        let frames = eg.frames();
        let st = eg.status();
        eg.stop()?;
        let cpu = cpu_s() - c0;
        let wall = t0.elapsed().as_secs_f64();
        println!(
            "webrtc{}: {frames} frames, process CPU {:.3} ms per 10 ms frame \
             ({:.1}% of one core in real time), AEC call {:.3} ms/frame [{wall:.1} s wall]",
            if guard { " + always guard" } else { "" },
            cpu / frames.max(1) as f64 * 1000.0,
            cpu / frames.max(1) as f64 / 0.01 * 100.0,
            st.aec_ms
        );
    }

    // memory: real time, full pipeline with guard
    let be = backend(1.0);
    let rss_before = status_kb("VmRSS:");
    let c0 = cpu_s();
    let eg = EchoGuard::start(&settings(true), be, log)?;
    std::thread::sleep(Duration::from_secs_f64(rss_seconds));
    let frames = eg.frames();
    let rss = status_kb("VmRSS:");
    let threads = std::fs::read_dir("/proc/self/task")
        .map(|d| d.count())
        .unwrap_or(0);
    eg.stop()?;
    let cpu = cpu_s() - c0;
    println!(
        "real time {rss_seconds:.0} s (webrtc + always guard): VmRSS {rss} kB \
         ({rss_before} kB before start, test signals included), VmHWM {} kB, \
         {threads} threads, CPU {:.3} ms per frame ({:.2}% of one core)",
        status_kb("VmHWM:"),
        cpu / frames.max(1) as f64 * 1000.0,
        cpu / rss_seconds * 100.0
    );
    Ok(())
}
