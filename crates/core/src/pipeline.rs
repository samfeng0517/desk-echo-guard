//! Real-time runner (`app.py`): threads that connect devices, buffers and DSP.
//!
//! ```text
//!     [speaker loopback] --ref_buf--+
//!                                   v
//!     [physical mic] -----------> AEC ---out_buf--> [CABLE Input]  (Discord/LINE mic = CABLE Output)
//!
//!     (siri_guard)
//!     [CABLE-A Output] --> SiriGuard (sibilant: ~60 ms lookahead) --spk_buf--> [speakers]
//!          ^ Discord/LINE output device = CABLE-A Input
//! ```
//!
//! The loopback of the real speakers captures the final speaker signal, i.e.
//! *after* the Siri guard and including every other app's sound, which is
//! exactly the echo reference the mic picks up.
//!
//! Every stream is owned by exactly one thread.  The GUI / CLI side only
//! reads atomics ([`EchoGuard::status`]), so polling never blocks audio.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Once, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use echoguard_aec::{make_engine, AecProcessor, EngineOptions};
use echoguard_audio::{AudioBackend, DeviceKind, InputStream, OutputStream};
use echoguard_dsp::{
    db, energy, estimate_delay_ms, Detector, ElasticBuffer, ElasticOptions, GuardConfig, GuardMode,
    NotchConfig, SibilantDetector, SiriGuard, FRAME, FS,
};
use parking_lot::Mutex;
use rand::SeedableRng;
use rand_distr::{Distribution, StandardNormal};

use crate::settings::Settings;
use crate::vcable;

/// Log line consumer (CLI: stdout, GUI: log view).
pub type LogSink = Arc<dyn Fn(String) + Send + Sync>;

/// How long `stop` waits for each pipeline thread.
const JOIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Level meters: exponential average of per-frame dB.
const LEVEL_ALPHA: f64 = 0.05;
const LEVEL_FLOOR_DB: f64 = -120.0;
/// Mic clipping warning threshold (peak, full scale = 1).
const CLIP_PEAK: f32 = 0.9;

/// Snapshot for the GUI meters / status line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub mic_db: f32,
    pub ref_db: f32,
    pub out_db: f32,
    /// reference elastic buffer fill
    pub ref_buf_ms: f32,
    /// clock drift estimate of the loopback vs. the mic
    pub drift_ppm: f32,
    pub ref_underruns: u64,
    pub out_underruns: u64,
    /// mean AEC processing time per 10 ms frame
    pub aec_ms: f32,
    pub siri_detections: u64,
    /// mic peak since the last `take_mic_peak`
    pub mic_peak: f32,
    /// first thread error (the pipeline has stopped)
    pub error: Option<String>,
}

// ------------------------------------------------------------------ helpers

/// f32 in an `AtomicU32`.
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
    fn swap(&self, v: f32) -> f32 {
        f32::from_bits(self.0.swap(v.to_bits(), Relaxed))
    }
    /// Max for non-negative values (their bit patterns order like the values).
    fn fetch_max_nonneg(&self, v: f32) {
        self.0.fetch_max(v.max(0.0).to_bits(), Relaxed);
    }
}

/// Seconds on a process-wide monotonic clock.
fn monotonic_s() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Bounded recorder for `debug_wav` (`app._Tap`).
struct Tap {
    max: usize,
    data: Mutex<Vec<f32>>,
}

impl Tap {
    fn new(max_s: f64) -> Self {
        Self {
            max: (max_s.max(0.0) * FS as f64) as usize,
            data: Mutex::new(Vec::new()),
        }
    }
    fn add(&self, x: &[f32]) {
        let mut d = self.data.lock();
        if d.len() < self.max {
            d.extend_from_slice(x);
        }
    }
}

struct Taps {
    mic: Tap,
    reference: Tap,
    out: Tap,
    call: Tap,
}

/// State shared between the pipeline threads and the controlling side.
struct Shared {
    stop: AtomicBool,
    failed: AtomicBool,
    error: Mutex<Option<anyhow::Error>>,
    ref_buf: ElasticBuffer,
    out_buf: ElasticBuffer,
    spk_buf: ElasticBuffer,
    mic_db: AtomicF32,
    ref_db: AtomicF32,
    out_db: AtomicF32,
    ref_level: AtomicF32,
    drift_ppm: AtomicF32,
    ref_underruns: AtomicU64,
    out_underruns: AtomicU64,
    frames: AtomicU64,
    proc_ns: AtomicU64,
    detections: AtomicU64,
    /// peak since `take_mic_peak` / since the last stats line
    peak_gui: AtomicF32,
    peak_stats: AtomicF32,
    taps: Option<Taps>,
}

impl Shared {
    fn new(s: &Settings, speed: f64) -> Self {
        // a simulated backend may run faster than real time; the buffers'
        // wall-clock interpolation must use the same time scale
        let clock: Option<echoguard_dsp::Clock> = if speed != 1.0 {
            Some(Arc::new(move || monotonic_s() * speed))
        } else {
            None
        };
        let ms = (FS / 1000) as f64;
        let buf = |target_ms: f64| {
            let opts = ElasticOptions {
                clock: clock.clone(),
                ..Default::default()
            };
            ElasticBuffer::with_options((target_ms * ms).max(0.0) as usize, s.max_ppm, opts)
        };
        let taps = s.debug_wav.as_ref().map(|_| Taps {
            mic: Tap::new(s.debug_max_s),
            reference: Tap::new(s.debug_max_s),
            out: Tap::new(s.debug_max_s),
            call: Tap::new(s.debug_max_s),
        });
        Self {
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            error: Mutex::new(None),
            ref_buf: buf(s.ref_buffer_ms),
            out_buf: buf(s.out_buffer_ms),
            spk_buf: buf(s.spk_buffer_ms),
            mic_db: AtomicF32::new(LEVEL_FLOOR_DB as f32),
            ref_db: AtomicF32::new(LEVEL_FLOOR_DB as f32),
            out_db: AtomicF32::new(LEVEL_FLOOR_DB as f32),
            ref_level: AtomicF32::new(0.0),
            drift_ppm: AtomicF32::new(0.0),
            ref_underruns: AtomicU64::new(0),
            out_underruns: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            proc_ns: AtomicU64::new(0),
            detections: AtomicU64::new(0),
            peak_gui: AtomicF32::new(0.0),
            peak_stats: AtomicF32::new(0.0),
            taps,
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Relaxed)
    }

    /// Record the first error and stop every thread (like `app._spawn`).
    fn fail(&self, e: anyhow::Error) {
        let mut g = self.error.lock();
        if g.is_none() {
            *g = Some(e);
        }
        self.failed.store(true, Relaxed);
        self.stop.store(true, Relaxed);
    }

    fn error_text(&self) -> Option<String> {
        if !self.failed.load(Relaxed) {
            return None;
        }
        self.error.lock().as_ref().map(|e| format!("{e:#}"))
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

/// Start a named pipeline thread; an error or panic stops the pipeline.
fn spawn(
    sh: &Arc<Shared>,
    threads: &mut Vec<JoinHandle<()>>,
    name: &'static str,
    f: impl FnOnce(&Shared) -> Result<()> + Send + 'static,
) -> Result<()> {
    let sh = sh.clone();
    let h = thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            // Audio deadlines: without this the thread runs at normal
            // priority and falls behind once the window is minimized.
            let _rt = echoguard_audio::realtime_thread();
            let r = match panic::catch_unwind(AssertUnwindSafe(|| f(&sh))) {
                Ok(r) => r,
                Err(p) => Err(anyhow!("panic: {}", panic_text(p.as_ref()))),
            };
            if let Err(e) = r {
                sh.fail(e.context(format!("thread {name}")));
            }
        })
        .with_context(|| format!("cannot start thread {name}"))?;
    threads.push(h);
    Ok(())
}

/// Set the stop flag and join every thread, waiting at most `JOIN_TIMEOUT`
/// for each; threads stuck in a device call are detached (they exit and
/// drop their stream once the call returns).
fn join_bounded(sh: &Shared, threads: &mut Vec<JoinHandle<()>>, log: &LogSink) {
    sh.stop.store(true, Relaxed);
    for h in threads.drain(..) {
        let deadline = Instant::now() + JOIN_TIMEOUT;
        while !h.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        if h.is_finished() {
            let _ = h.join();
        } else {
            let name = h.thread().name().unwrap_or("?").to_string();
            log(format!("[stop] thread {name} did not stop within 2 s"));
        }
    }
}

// ------------------------------------------------------------------ threads

/// Device -> elastic buffer (loopback reference).
fn ref_reader(sh: &Shared, mut stream: Box<dyn InputStream>, blocksize: usize) -> Result<()> {
    let mut x = vec![0.0f32; blocksize];
    while !sh.stopping() {
        stream.read(&mut x)?;
        sh.ref_buf.push(&x);
    }
    Ok(())
}

/// Call audio -> Siri guard -> speaker buffer.
fn guard_reader(
    sh: &Shared,
    mut stream: Box<dyn InputStream>,
    mut guard: SiriGuard,
    blocksize: usize,
) -> Result<()> {
    let mut x = vec![0.0f32; blocksize];
    let mut y = vec![0.0f32; blocksize];
    while !sh.stopping() {
        stream.read(&mut x)?;
        guard.process(&x, &mut y);
        sh.detections.store(guard.detections(), Relaxed);
        if let Some(t) = &sh.taps {
            t.call.add(&y);
        }
        sh.spk_buf.push(&y);
    }
    Ok(())
}

/// Elastic buffer -> device.
fn writer(
    sh: &Shared,
    mut stream: Box<dyn OutputStream>,
    buf: fn(&Shared) -> &ElasticBuffer,
    blocksize: usize,
) -> Result<()> {
    let mut y = vec![0.0f32; blocksize];
    while !sh.stopping() {
        buf(sh).pop_into(&mut y);
        stream.write(&y)?;
    }
    Ok(())
}

/// Mic -> AEC (with the reference from `ref_buf`) -> `out_buf`.
fn aec_loop(
    sh: &Shared,
    mut mic: Box<dyn InputStream>,
    mut aec: AecProcessor,
    blocksize: usize,
) -> Result<()> {
    let mut block = vec![0.0f32; blocksize];
    let mut acc: Vec<f32> = Vec::with_capacity(blocksize + FRAME);
    let mut r = vec![0.0f32; FRAME];
    let mut y = vec![0.0f32; FRAME];
    let mut levels = [LEVEL_FLOOR_DB; 3];
    while !sh.stopping() {
        mic.read(&mut block)?;
        acc.extend_from_slice(&block);
        let mut off = 0;
        while acc.len() - off >= FRAME {
            let m = &acc[off..off + FRAME];
            off += FRAME;
            sh.ref_buf.pop_into(&mut r);
            let t0 = Instant::now();
            aec.process(m, &r, &mut y)?;
            sh.proc_ns
                .fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
            sh.frames.fetch_add(1, Relaxed);
            sh.out_buf.push(&y);

            let peak = m.iter().fold(0.0f32, |p, v| p.max(v.abs()));
            sh.peak_gui.fetch_max_nonneg(peak);
            sh.peak_stats.fetch_max_nonneg(peak);
            for (l, v) in levels.iter_mut().zip([m, &r[..], &y[..]]) {
                *l = (1.0 - LEVEL_ALPHA) * *l + LEVEL_ALPHA * db(energy(v));
            }
            sh.mic_db.store(levels[0] as f32);
            sh.ref_db.store(levels[1] as f32);
            sh.out_db.store(levels[2] as f32);
            let rs = sh.ref_buf.stats();
            sh.ref_level.store(rs.level as f32);
            sh.drift_ppm.store(rs.drift_ppm as f32);
            sh.ref_underruns.store(rs.underruns, Relaxed);
            sh.out_underruns.store(sh.out_buf.underruns(), Relaxed);
            if let Some(t) = &sh.taps {
                t.mic.add(m);
                t.reference.add(&r);
                t.out.add(&y);
            }
        }
        acc.drain(..off);
    }
    Ok(())
}

// ----------------------------------------------------------------- pipeline

/// Build the Siri guard from the settings (`siri_guard.make_guard`):
/// `sibilant` uses the built-in /s/ detector (lookahead derived from it),
/// `always` notches all the time.
pub fn make_guard(s: &Settings) -> Result<SiriGuard> {
    let notch = NotchConfig::profile(&s.siri_profile, s.siri_depth_db)?;
    let mode: GuardMode = s.siri_mode.parse()?;
    let cfg = GuardConfig {
        mode,
        notch,
        ..Default::default()
    };
    let det: Option<Box<dyn Detector>> = match mode {
        GuardMode::Sibilant => Some(Box::new(SibilantDetector::default())),
        GuardMode::Always | GuardMode::Off => None,
    };
    SiriGuard::new(cfg, det)
}

/// A running pipeline (`app.EchoGuardApp`).  Dropping it stops it too
/// (without writing the debug wavs).
pub struct EchoGuard {
    sh: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    engine: &'static str,
    guard_latency_ms: Option<f64>,
    debug_dir: Option<PathBuf>,
    log: LogSink,
}

impl EchoGuard {
    /// Build the DSP, open the devices and start the threads.
    pub fn start(s: &Settings, backend: Arc<dyn AudioBackend>, log: LogSink) -> Result<EchoGuard> {
        // keep full speed when minimized / in the tray (Windows EcoQoS)
        echoguard_audio::disable_power_throttling();
        let opts = EngineOptions {
            noise_suppression: s.noise_suppression,
            high_pass_filter: s.high_pass,
            auto_gain_control: s.agc,
            filter_ms: s.filter_ms,
        };
        let engine = make_engine(&s.engine, &opts)?;
        let aec = AecProcessor::new(engine, s.delay_ms, s.mic_delay_ms, s.output_gain_db);
        let engine = aec.engine_name();
        log(format!(
            "[aec] engine={engine} ref_delay={:?} ms mic_delay={:?} ms",
            s.delay_ms, s.mic_delay_ms
        ));
        let guard = if s.siri_guard {
            let g = make_guard(s)?;
            let n = g.notch_config();
            log(format!(
                "[siri-guard] mode={} profile={} bands={:?} depth={:?} dB latency={:.0} ms",
                s.siri_mode,
                s.siri_profile,
                n.bands,
                n.depth_db,
                g.latency() as f64 / FS as f64 * 1000.0
            ));
            Some(g)
        } else {
            None
        };
        let guard_latency_ms = guard
            .as_ref()
            .map(|g| g.latency() as f64 / FS as f64 * 1000.0);

        // "CABLE Input" keeps working after vcable renamed the endpoints
        let devs = backend.list_devices().unwrap_or_default();
        let out_name = vcable::resolve(&s.out, DeviceKind::Speaker, &devs);
        let call_name = vcable::resolve(&s.call_in, DeviceKind::Mic, &devs);
        let bs = s.blocksize.max(1);
        let mic = backend.open_input(s.mic.as_deref(), false, bs)?;
        let lb = backend.open_input(s.speaker.as_deref(), true, bs)?;
        let out = backend.open_output(Some(&out_name), bs)?;
        let guard_io = match guard {
            Some(g) => {
                let call = backend.open_input(Some(&call_name), false, bs)?;
                let spk = backend.open_output(s.speaker.as_deref(), bs)?;
                Some((g, call, spk))
            }
            None => None,
        };

        let sh = Arc::new(Shared::new(s, backend.speed()));
        let mut eg = EchoGuard {
            sh: sh.clone(),
            threads: Vec::new(),
            engine,
            guard_latency_ms,
            debug_dir: s.debug_wav.as_ref().map(PathBuf::from),
            log,
        };
        let th = &mut eg.threads;
        spawn(&sh, th, "ref-reader", move |sh| ref_reader(sh, lb, bs))?;
        spawn(&sh, th, "out-writer", move |sh| {
            writer(sh, out, |sh| &sh.out_buf, bs)
        })?;
        if let Some((g, call, spk)) = guard_io {
            spawn(&sh, th, "guard-reader", move |sh| {
                guard_reader(sh, call, g, bs)
            })?;
            spawn(&sh, th, "spk-writer", move |sh| {
                writer(sh, spk, |sh| &sh.spk_buf, bs)
            })?;
        }
        spawn(&sh, th, "aec", move |sh| aec_loop(sh, mic, aec, bs))?;
        Ok(eg)
    }

    /// Lock-free snapshot (atomics written once per frame by the AEC thread).
    pub fn status(&self) -> Status {
        let sh = &self.sh;
        Status {
            mic_db: sh.mic_db.load(),
            ref_db: sh.ref_db.load(),
            out_db: sh.out_db.load(),
            ref_buf_ms: sh.ref_level.load() / (FS / 1000) as f32,
            drift_ppm: sh.drift_ppm.load(),
            ref_underruns: sh.ref_underruns.load(Relaxed),
            out_underruns: sh.out_underruns.load(Relaxed),
            aec_ms: self.aec_ms() as f32,
            siri_detections: sh.detections.load(Relaxed),
            mic_peak: sh.peak_gui.load(),
            error: sh.error_text(),
        }
    }

    /// Mic peak since the last call, then reset.
    pub fn take_mic_peak(&self) -> f32 {
        self.sh.peak_gui.swap(0.0)
    }

    /// AEC frames processed so far.
    pub fn frames(&self) -> u64 {
        self.sh.frames.load(Relaxed)
    }

    fn aec_ms(&self) -> f64 {
        let frames = self.sh.frames.load(Relaxed).max(1);
        self.sh.proc_ns.load(Relaxed) as f64 / frames as f64 / 1e6
    }

    /// CLI statistics line (`app.stats_line`); resets the clipping check.
    pub fn stats_line(&self) -> String {
        let sh = &self.sh;
        let (r, o) = (sh.ref_buf.stats(), sh.out_buf.stats());
        let mut line = format!(
            "mic {:6.1} dB  ref {:6.1} dB  out {:6.1} dB | ref buf {:5.1} ms {:+6.0} ppm ur={} | \
             out buf {:5.1} ms ur={} | aec {:.2} ms/10ms",
            sh.mic_db.load(),
            sh.ref_db.load(),
            sh.out_db.load(),
            r.level / 48.0,
            r.ppm,
            r.underruns,
            o.level / 48.0,
            o.underruns,
            self.aec_ms()
        );
        if self.guard_latency_ms.is_some() {
            line += &format!(" | siri detections={}", sh.detections.load(Relaxed));
        }
        if sh.peak_stats.swap(0.0) > CLIP_PEAK {
            line += "\n  ! mic peaks near full scale - lower the mic gain in Windows; \
                     AEC3 stops cancelling on saturated input";
        }
        line
    }

    pub fn engine_name(&self) -> &str {
        self.engine
    }

    /// Siri guard latency (notch delay + lookahead); `None` without guard.
    pub fn guard_latency_ms(&self) -> Option<f64> {
        self.guard_latency_ms
    }

    /// A thread failed; the pipeline has stopped (see `status().error`).
    pub fn is_failed(&self) -> bool {
        self.sh.failed.load(Relaxed)
    }

    /// The pipeline stopped (thread error) or is stopping.
    pub fn is_stopped(&self) -> bool {
        self.sh.stopping()
    }

    /// Stop the threads (bounded wait), close the devices and write the
    /// debug wavs.  Returns the first thread error, if any.
    pub fn stop(mut self) -> Result<()> {
        join_bounded(&self.sh, &mut self.threads, &self.log);
        let wav = match (&self.debug_dir, &self.sh.taps) {
            (Some(d), Some(t)) => write_debug(d, t, &self.log),
            _ => Ok(()),
        };
        if let Some(e) = self.sh.error.lock().take() {
            return Err(e);
        }
        wav
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            join_bounded(&self.sh, &mut self.threads, &self.log);
        }
    }
}

/// Write the recorded taps as 16-bit wavs (`app.write_debug`).
fn write_debug(dir: &Path, taps: &Taps, log: &LogSink) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: FS as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    for (name, tap) in [
        ("mic", &taps.mic),
        ("ref", &taps.reference),
        ("out", &taps.out),
        ("call", &taps.call),
    ] {
        let data = std::mem::take(&mut *tap.data.lock());
        if data.is_empty() {
            continue;
        }
        let path = dir.join(format!("{name}.wav"));
        let mut w = hound::WavWriter::create(&path, spec)
            .with_context(|| format!("cannot write {}", path.display()))?;
        for v in data {
            w.write_sample((v * 32767.0).clamp(-32768.0, 32767.0) as i16)?;
        }
        w.finalize()?;
    }
    log(format!("[debug] wrote wavs to {}", dir.display()));
    Ok(())
}

/// Set by the Ctrl+C handler (installed once per process).
fn ctrl_c_flag() -> &'static AtomicBool {
    static FLAG: AtomicBool = AtomicBool::new(false);
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if let Err(e) = ctrlc::set_handler(|| FLAG.store(true, Relaxed)) {
            log::warn!("cannot install the Ctrl+C handler: {e}");
        }
    });
    &FLAG
}

/// CLI loop (`app.run`): start, print stats every `stats_interval_s`, stop
/// after `duration_s` or on Ctrl+C.  Returns the first thread error.
pub fn run_blocking(
    s: &Settings,
    backend: Arc<dyn AudioBackend>,
    duration_s: Option<f64>,
    log: LogSink,
) -> Result<()> {
    let ctrl_c = ctrl_c_flag();
    ctrl_c.store(false, Relaxed);
    let eg = EchoGuard::start(s, backend, log.clone())?;
    let t0 = Instant::now();
    let mut last = t0;
    while !eg.is_stopped() {
        thread::sleep(Duration::from_millis(100));
        if ctrl_c.swap(false, Relaxed) {
            log("stopping ...".into());
            break;
        }
        let now = Instant::now();
        if s.stats_interval_s > 0.0 && (now - last).as_secs_f64() >= s.stats_interval_s {
            last = now;
            log(eg.stats_line());
        }
        if duration_s.is_some_and(|d| (now - t0).as_secs_f64() >= d) {
            break;
        }
    }
    eg.stop()
}

/// Play noise bursts on the speakers, record mic and loopback through the
/// same buffering the AEC uses, and estimate the ref -> mic delay
/// (`app.calibrate`).  Returns `(delay_ms, confidence)`.
pub fn calibrate(
    settings: &Settings,
    backend: Arc<dyn AudioBackend>,
    seconds: f64,
    level_db: f64,
    log: LogSink,
) -> Result<(f64, f64)> {
    let s = Settings {
        engine: "off".into(),
        siri_guard: false,
        debug_wav: None,
        ..settings.clone()
    };
    let bs = s.blocksize.max(1);
    let n = (seconds.max(0.0) * FS as f64) as usize;
    let mut rng = rand::rngs::StdRng::seed_from_u64(0);
    let amp = 10f64.powf(level_db / 20.0);
    let noise: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 / FS as f64;
            // on/off pattern
            let on = (2.0 * std::f64::consts::PI * 2.0 * t).sin() > 0.3;
            let v: f64 = StandardNormal.sample(&mut rng);
            if on {
                (v * amp) as f32
            } else {
                0.0
            }
        })
        .collect();

    let sh = Arc::new(Shared::new(&s, backend.speed()));
    let quiet: LogSink = Arc::new(|_| {});
    let mut threads = Vec::new();
    let mut mic = backend.open_input(s.mic.as_deref(), false, bs)?;
    let lb = backend.open_input(s.speaker.as_deref(), true, bs)?;
    let result = (|| -> Result<(Vec<f32>, Vec<f32>)> {
        spawn(&sh, &mut threads, "ref-reader", move |sh| {
            ref_reader(sh, lb, bs)
        })?;
        thread::sleep(Duration::from_millis(200));
        let be = backend.clone();
        let speaker = s.speaker.clone();
        spawn(&sh, &mut threads, "player", move |sh| {
            let mut spk = be.open_output(speaker.as_deref(), bs)?;
            for chunk in noise.chunks(bs) {
                if sh.stopping() {
                    break;
                }
                spk.write(chunk)?;
            }
            Ok(())
        })?;
        let total = ((seconds + 0.6) * FS as f64) as usize;
        let frames = total.div_ceil(FRAME);
        let mut mics = vec![0.0f32; frames * FRAME];
        let mut refs = vec![0.0f32; frames * FRAME];
        let mut got = 0;
        while got < total && !sh.stopping() {
            mic.read(&mut mics[got..got + FRAME])?;
            sh.ref_buf.pop_into(&mut refs[got..got + FRAME]);
            got += FRAME;
        }
        mics.truncate(got);
        refs.truncate(got);
        Ok((mics, refs))
    })();
    join_bounded(&sh, &mut threads, &quiet);
    drop(mic);
    if let Some(e) = sh.error.lock().take() {
        return Err(e);
    }
    let (m, r) = result?;
    if db(energy(&r)) < -70.0 {
        log("[calibrate] loopback stayed silent - is --speaker the device that is playing?".into());
    }
    let (d, conf) = estimate_delay_ms(&r, &m, 500.0, None);
    log(format!(
        "[calibrate] ref->mic delay = {d:.1} ms (confidence {conf:.1})"
    ));
    Ok((d, conf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_f32_max_and_swap() {
        let a = AtomicF32::new(0.0);
        a.fetch_max_nonneg(0.25);
        a.fetch_max_nonneg(0.1);
        assert_eq!(a.load(), 0.25);
        assert_eq!(a.swap(0.0), 0.25);
        assert_eq!(a.load(), 0.0);
    }

    #[test]
    fn tap_is_bounded() {
        let t = Tap::new(0.01); // 480 samples
        t.add(&[0.0; 400]);
        t.add(&[0.0; 400]);
        t.add(&[0.0; 400]);
        assert_eq!(t.data.lock().len(), 800);
    }

    #[test]
    fn make_guard_modes_and_errors() {
        let mut s = Settings {
            siri_mode: "always".into(),
            ..Default::default()
        };
        let g = make_guard(&s).unwrap();
        assert!(g.latency() < FS / 10);
        s.siri_mode = "sibilant".into();
        let g = make_guard(&s).unwrap();
        assert!(
            g.latency() > 600 && g.latency() < FS / 10,
            "{}",
            g.latency()
        );
        s.siri_mode = "detect".into(); // removed in 0.4.1 (settings map it)
        assert!(make_guard(&s).is_err());
        s.siri_mode = "bogus".into();
        assert!(make_guard(&s).is_err());
        s.siri_mode = "always".into();
        s.siri_profile = "nope".into();
        assert!(make_guard(&s).is_err());
    }
}
