# desk-echo-guard (Rust) — design and porting contract

This directory is a full Rust rewrite of the Python program in `../echoguard`.
Goal: **feature parity** with the Python version (same settings file, same
CLI options, same DSP behaviour and test thresholds), much lower RAM / CPU, a
single fast-starting exe, and a redesigned Windows 11 style GUI.

The Python code is the *reference implementation*: when this document and the
Python code disagree about DSP behaviour, the Python code wins.  Port its
tests too (same scenarios, same pass thresholds).

## Crates

| crate | Python counterpart | contents |
|---|---|---|
| `echoguard-dsp` | `buffers.py` `resample.py` `delay.py` `siri_guard.py` (DSP parts) `aec.py` (NLMS) `signals.py` `metrics.py` | pure Rust, no OS deps, everything testable on Linux |
| `echoguard-aec` | `aec.py` | `EchoCanceller` trait, engines webrtc / speex / nlms / off, `AecProcessor` |
| `echoguard-wakeword` | `siri_guard.OpenWakeWordDetector` | openWakeWord pipeline on a small built-in pure-Rust ONNX interpreter (float32 DNN models; no ONNX Runtime) |
| `echoguard-audio` | `io_backend.py` `sim.py` | device traits, WASAPI backend (Windows), simulated backend |
| `echoguard-core` | `app.py` `cli.py` (settings) `vcable.py` `winutil.py` `selftest.py` | settings / TOML, real-time pipeline, calibration, VB-Cable, autostart, self-test |
| `desk-echo-guard` (crates/app) | `gui.py` `tray.py` `cli.py` | the exe: GUI (Slint, Fluent style), tray, CLI |

Dependency direction: `dsp` <- `aec`, `wakeword`; `audio` (independent);
`core` <- all of the above; `app` <- `core`.

Rules for every crate:

* Mono `f32` at 48 kHz everywhere (`dsp::FS = 48_000`, `dsp::FRAME = 480`).
* Library errors: `anyhow::Result`.  No `unwrap()` on anything that can fail
  at run time (devices, files, user input).
* No allocation-heavy work in per-frame paths where it is easy to avoid
  (prefer `_into(&mut [f32])` variants and reused buffers).
* Unit tests live next to the code; integration tests in `tests/`.
* Windows-only code behind `#[cfg(windows)]`, with a stub (returning an
  error or doing nothing) elsewhere so the workspace builds and tests on
  Linux.
* Comments in English, user-facing strings in Traditional Chinese (Taiwan).

## Public API contract (other crates build against these)

### echoguard-dsp

```rust
pub const FS: usize = 48_000;
pub const FRAME: usize = 480;

// elastic.rs  (buffers.ElasticBuffer) – thread safe (&self, internal Mutex)
pub type Clock = std::sync::Arc<dyn Fn() -> f64 + Send + Sync>;   // seconds
pub struct ElasticBuffer;
impl ElasticBuffer {
    pub fn new(target: usize, max_ppm: f64) -> Self;            // real-time clock
    pub fn with_options(target: usize, max_ppm: f64, opts: ElasticOptions) -> Self;
    pub fn push(&self, x: &[f32]);
    pub fn push_at(&self, x: &[f32], now: f64);
    pub fn pop_into(&self, out: &mut [f32]);
    pub fn pop_at(&self, out: &mut [f32], now: f64);
    pub fn level(&self) -> f64;
    pub fn stats(&self) -> ElasticStats;   // level, ppm, drift_ppm, underruns, resyncs
}
pub struct ElasticOptions { pub hard_excess: Option<usize>, pub tau_s: f64,
                            pub smooth_s: f64, pub clock: Option<Clock> }   // Default impl
pub struct ElasticStats { pub level: f64, pub ppm: f64, pub drift_ppm: f64,
                          pub underruns: u64, pub resyncs: u64 }

// delay_line.rs (buffers.DelayLine) – ring buffer
pub struct DelayLine;  new(delay: usize); process(&mut self, x: &[f32], out: &mut [f32]); delay()

// resample.rs (resample.IntegerResampler) – polyphase, state kept across calls
pub struct IntegerResampler;  new(factor: i32) (>0 down, <0 up); process(&mut self, x: &[f32]) -> Vec<f32>; delay()

// delay_est.rs (delay.estimate_delay_ms)
pub fn estimate_delay_ms(reference: &[f32], mic: &[f32], max_delay_ms: f64,
                         min_delay_ms: Option<f64>) -> (f64, f64); // (delay_ms, confidence)

// notch.rs (siri_guard.NotchConfig / PROFILES / NotchBank / design_notch_fir)
pub struct NotchConfig { pub bands: Vec<(f64, f64)>, pub depth_db: f64,
                         pub transition_hz: f64, pub numtaps: usize }
impl NotchConfig { pub fn profile(name: &str, depth_db: Option<f64>) -> anyhow::Result<Self>; }
pub const PROFILES: &[&str] = &["deep-wide", "apple-deep", "apple-wide", "formant"];
pub const DEFAULT_PROFILE: &str = "deep-wide";   // legacy "apple" maps to it
pub fn design_notch_fir(cfg: &NotchConfig) -> Vec<f64>;
pub struct NotchBank;  new(cfg); process(&mut self, x: &[f32], out: &mut [f32]); delay()

// guard.rs (siri_guard.SiriGuard, detectors)
pub trait Detector: Send {   // regions in absolute input-sample indices
    fn feed(&mut self, x: &[f32], start: i64) -> Vec<(i64, i64)>;
}
pub struct OracleDetector; pub struct SibilantDetector;   // both impl Detector
pub enum GuardMode { Always, Sibilant, Detect, Off }       // FromStr / as_str
pub struct GuardConfig { pub mode: GuardMode, pub lookahead_s: f64, pub ramp_ms: f64,
                         pub notch: NotchConfig }
pub struct SiriGuard;
impl SiriGuard {
    pub fn new(cfg: GuardConfig, detector: Option<Box<dyn Detector>>) -> anyhow::Result<Self>;
    pub fn process(&mut self, x: &[f32], out: &mut [f32]);
    pub fn process_signal(&mut self, x: &[f32], block: usize) -> Vec<f32>; // latency removed
    pub fn latency(&self) -> usize; pub fn detections(&self) -> u64;
}

// nlms.rs (aec.NlmsAec.process_lowrate): 16 kHz, 160-sample blocks
pub struct Nlms16k;  new(filter_ms: u32); process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32])

// testsig.rs (signals.py) and metrics.rs (metrics.py) – also used by the
// self-test at run time, so they are normal modules, not test-only:
pub fn synth_speech(dur_s: f64, seed: u64) -> Vec<f32>;
pub struct Scenario { pub mic, pub reference, pub near, pub echo: Vec<f32>,
                      pub far_only, pub near_only, pub double_talk: Vec<bool> }
pub fn make_scenario(dur_s: f64, delay_ms: f64, seed: u64) -> Scenario;  // see signals.make_scenario
pub fn db(x: f64) -> f64; pub fn erle_db(...); pub fn band_energy_db(...); pub fn best_lag(...);
```

Random signals do not need to be bit-identical with numpy; the *statistics*
must match so that the ported tests pass with the same thresholds.

### echoguard-aec

```rust
pub struct EngineOptions { pub noise_suppression: bool, pub high_pass_filter: bool,
                           pub auto_gain_control: bool, pub filter_ms: u32 }  // Default: true,true,false,300
pub trait EchoCanceller: Send {
    fn name(&self) -> &'static str;
    /// 10 ms @ 48 kHz in, 10 ms @ 48 kHz out
    fn process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> anyhow::Result<()>;
}
pub const ENGINES: &[&str] = &["webrtc", "speex", "nlms", "off"];
pub fn available_engines() -> Vec<&'static str>;
pub fn make_engine(name: &str, opts: &EngineOptions) -> anyhow::Result<Box<dyn EchoCanceller>>; // "auto" = first available
pub struct AecProcessor;
impl AecProcessor {
    pub fn new(engine: Box<dyn EchoCanceller>, ref_delay_ms: f64, mic_delay_ms: f64, output_gain_db: f64) -> Self;
    pub fn process(&mut self, mic: &[f32], reference: &[f32], out: &mut [f32]) -> anyhow::Result<()>;
    pub fn process_signal(&mut self, mic: &[f32], reference: &[f32]) -> anyhow::Result<Vec<f32>>;
    pub fn engine_name(&self) -> &'static str;
}
```

* `webrtc`: crate `libwebrtc` (LiveKit, the same WebRTC build the Python
  `livekit` wheel uses) – `native::apm::AudioProcessingModule`; i16
  conversion exactly like `aec._to_i16` / `_from_i16`; call order
  reverse, `set_stream_delay_ms(0)`, process.  Cargo feature `webrtc`
  (default on).
* `speex`: SpeexDSP echo canceller + preprocessor (residual echo
  suppression) as in the `pyaec` wheel; vendored C sources built with `cc`.
  Feature `speex` (default on).

### echoguard-wakeword

Inference runs on the crate's own ONNX interpreter (`onnx.rs` parser, `nn.rs` ops, `gemm.rs` AVX2/portable kernel): ~0.3 MB instead of ONNX Runtime's ~24 MB + DirectML / VC++ DLLs.  Unsupported ops (e.g. LSTM heads) fail at load time with the op name.

```rust
pub struct OpenWakeWordDetector;   // impl echoguard_dsp::Detector
impl OpenWakeWordDetector {
    pub fn new(model_paths: &[std::path::PathBuf], threshold: f32, pre_s: f64,
               post_s: f64, refractory_s: f64) -> anyhow::Result<Self>;
}
pub fn feature_model_paths(model_paths: &[PathBuf]) -> anyhow::Result<(PathBuf, PathBuf)>; // melspec, embedding (download if missing)
```

### echoguard-audio

```rust
pub enum DeviceKind { Mic, Speaker }
pub struct DeviceInfo { pub kind: DeviceKind, pub name: String, pub id: String,
                        pub channels: u16, pub default: bool }
pub trait InputStream: Send { fn read(&mut self, out: &mut [f32]) -> anyhow::Result<()>; } // blocks until full
pub trait OutputStream: Send { fn write(&mut self, x: &[f32]) -> anyhow::Result<()>; }    // blocks until queued
pub trait AudioBackend: Send + Sync {
    fn list_devices(&self) -> anyhow::Result<Vec<DeviceInfo>>;
    fn open_input(&self, name: Option<&str>, loopback: bool, blocksize: usize) -> anyhow::Result<Box<dyn InputStream>>;
    fn open_output(&self, name: Option<&str>, blocksize: usize) -> anyhow::Result<Box<dyn OutputStream>>;
    fn speed(&self) -> f64 { 1.0 }   // SimBackend: faster than real time
}
pub fn match_device<'a>(items: &'a [DeviceInfo], query: &str, what: &str) -> anyhow::Result<&'a DeviceInfo>;
pub mod sim;     // SimBackend / SimInput / SimOutput (sim.py)
#[cfg(windows)] pub mod wasapi;   // WasapiBackend: event-driven shared mode, loopback, no polling
pub fn default_backend() -> anyhow::Result<std::sync::Arc<dyn AudioBackend>>;
```

The WASAPI backend must not poll: each stream runs an event-driven capture /
render thread (`wasapi` crate) feeding a lock-free ring buffer; `read` /
`write` block on it.  Multi-channel devices are down-mixed / duplicated.
Loopback endpoints that deliver no packets while nothing plays must still
produce silence at the nominal rate (like soundcard's idle handling).

### echoguard-core

```rust
// settings.rs (app.Settings + cli load/save) – identical keys and defaults
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings { pub mic: Option<String>, pub speaker: Option<String>, pub out: String,
    pub engine: String, pub noise_suppression: bool, pub high_pass: bool, pub agc: bool,
    pub filter_ms: u32, pub delay_ms: f64, pub mic_delay_ms: f64, pub output_gain_db: f64,
    pub ref_buffer_ms: f64, pub out_buffer_ms: f64, pub max_ppm: f64, pub blocksize: usize,
    pub siri_guard: bool, pub call_in: String, pub siri_mode: String, pub siri_model: Vec<String>,
    pub siri_threshold: f64, pub siri_profile: String, pub siri_depth_db: Option<f64>,
    pub siri_lookahead_s: f64, pub spk_buffer_ms: f64, pub debug_wav: Option<String>,
    pub debug_max_s: f64, pub stats_interval_s: f64 }
pub fn default_config_path() -> PathBuf;                 // %APPDATA%\desk-echo-guard\echoguard.toml
pub fn load_config(path) -> Result<toml::Table>;          // flattens [tables], '-' -> '_'
pub fn settings_from_table(t, strict: bool) -> Result<Settings>;  // strict = unknown key is an error (CLI)
pub fn save_config(path, &Settings) -> Result<()>;       // flat TOML, readable by the Python version too

// pipeline.rs (app.EchoGuardApp)
pub type LogSink = Arc<dyn Fn(String) + Send + Sync>;
pub struct Status { pub mic_db: f32, pub ref_db: f32, pub out_db: f32, pub ref_buf_ms: f32,
    pub drift_ppm: f32, pub ref_underruns: u64, pub out_underruns: u64, pub aec_ms: f32,
    pub siri_detections: u64, pub mic_peak: f32, pub error: Option<String> }
pub struct EchoGuard;
impl EchoGuard {
    pub fn start(s: &Settings, backend: Arc<dyn AudioBackend>, log: LogSink) -> Result<EchoGuard>;
    pub fn status(&self) -> Status;        // cheap, lock-free; GUI polls it
    pub fn take_mic_peak(&self) -> f32;    // peak since last call, then reset
    pub fn stats_line(&self) -> String;    // CLI line, as app.stats_line
    pub fn engine_name(&self) -> &str;
    pub fn guard_latency_ms(&self) -> Option<f64>;
    pub fn is_failed(&self) -> bool;
    pub fn stop(self) -> Result<()>;       // joins threads (bounded wait), writes debug wavs
}
pub fn run_blocking(s: &Settings, backend, duration_s: Option<f64>, log) -> Result<()>; // CLI loop + Ctrl+C
pub fn calibrate(s: &Settings, backend, seconds: f64, level_db: f64, log) -> Result<(f64, f64)>;
pub fn make_guard(s: &Settings) -> Result<SiriGuard>;    // detect -> wakeword, sibilant, always

// vcable.rs (vcable.py) – pure role logic + Windows endpoint ops
pub struct Role { pub key: &'static str, pub kind: DeviceKind, pub original: &'static str, pub renamed: &'static str }
pub const ROLES: [Role; 4]; pub const DOWNLOAD_URL: &str;
pub fn role_of(name, kind: Option<DeviceKind>) -> Option<&'static Role>;
pub fn find(devices, role) -> Option<String>; pub fn resolve(name, kind, devices) -> String;
pub struct CableStatus { pub found: bool, pub renamed: bool, pub names: BTreeMap<&'static str, Option<String>>, pub unused: Vec<String> }
pub fn status(devices) -> CableStatus; pub fn display_name(devices, key) -> String;
pub enum CableAction { Rename, Hide, Restore }
pub fn apply(actions: &[CableAction]) -> Result<Vec<String>>;          // needs admin (Windows)
pub fn run_elevated(actions: &[CableAction]) -> (bool, String);         // UAC: re-runs this exe with `--vcable a,b RESULT.json`
pub fn elevated_main(args: &[String]) -> i32;

// winutil.rs – autostart (HKCU Run "DeskEchoGuard", value `"<exe>" --autostart`),
// single instance (named mutex Local\DeskEchoGuard.SingleInstance), refresh_autostart()
// selftest.rs – `--self-test [report.txt]`, same checks as selftest.py
```

### desk-echo-guard (crates/app)

* `main.rs`: no arguments (or `--config`, `--autostart`) -> GUI;
  CLI options identical to `cli.py` (clap) -> console mode (attach to the
  parent console on Windows); `--vcable ACTIONS RESULT.json` -> elevated
  helper; `--self-test`.
* GUI: Slint with the `fluent` style (Windows 11 look), Mica backdrop when
  available, Segoe UI Variable, follows the system light/dark theme, DPI
  aware.  See `crates/app/UI.md` for the screen-by-screen spec.
* Tray: `tray-icon` (show window / start-stop / quit); closing the window
  hides to tray, as today.

## Behavioural details that are easy to get wrong

* ElasticBuffer: continuous level estimate from push timestamps, critically
  damped PI controller, anti-windup only while not underrunning, hard resync,
  re-prime after a long producer stall, underrun keeps the read position
  advancing (see the docstring in `buffers.py`).
* SiriGuard always mode = notch only, no lookahead; detect / sibilant =
  1 s lookahead, cross-fade mask with raised-cosine ramps, regions clamped
  to what is still in the buffer.
* Pipeline levels: exponential average (a = 0.05) of per-frame dB, start at
  -120; mic clipping warning above 0.9 peak.
* `vcable.resolve`: configured "CABLE Input" must keep working after rename.
* Settings file: missing keys take defaults; `siri_model` may be a string.
