//! Settings and the TOML settings file (`app.Settings`, `cli.load_config`,
//! `cli.save_config`, `gui.default_config_path`).
//!
//! The file format is shared with the Python version: a flat TOML file
//! (optionally grouped into `[tables]`, which are flattened one level), keys
//! are the long CLI option names with `-` or `_`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use echoguard_dsp::DEFAULT_PROFILE;

/// The legacy profile name "apple" maps to the default notch profile
/// (`siri_guard.LEGACY_PROFILES`).
const LEGACY_PROFILES: &[(&str, &str)] = &[("apple", DEFAULT_PROFILE)];

/// Every setting of the program; field names, order and defaults are those
/// of `app.Settings`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// physical mic (substring); None = default
    pub mic: Option<String>,
    /// real speakers (loopback ref + guard output); None = default
    pub speaker: Option<String>,
    /// virtual mic sink
    pub out: String,
    /// webrtc | speex | nlms | off | auto
    pub engine: String,
    pub noise_suppression: bool,
    pub high_pass: bool,
    pub agc: bool,
    /// speex / nlms filter length
    pub filter_ms: u32,
    /// extra delay applied to the reference
    pub delay_ms: f64,
    /// extra delay applied to the mic
    pub mic_delay_ms: f64,
    pub output_gain_db: f64,
    /// elastic buffer targets (= added latency)
    pub ref_buffer_ms: f64,
    pub out_buffer_ms: f64,
    pub max_ppm: f64,
    /// device block size request (samples)
    pub blocksize: usize,
    pub siri_guard: bool,
    /// where Discord/LINE output arrives
    pub call_in: String,
    /// always | sibilant | detect (needs siri_model)
    pub siri_mode: String,
    pub siri_model: Vec<String>,
    pub siri_threshold: f64,
    pub siri_profile: String,
    pub siri_depth_db: Option<f64>,
    pub siri_lookahead_s: f64,
    pub spk_buffer_ms: f64,
    /// directory: dump mic/ref/out(/call) wavs
    pub debug_wav: Option<String>,
    pub debug_max_s: f64,
    pub stats_interval_s: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            mic: None,
            speaker: None,
            out: "CABLE Input".into(),
            engine: "auto".into(),
            noise_suppression: true,
            high_pass: true,
            agc: false,
            filter_ms: 300,
            delay_ms: 0.0,
            mic_delay_ms: 0.0,
            output_gain_db: 0.0,
            ref_buffer_ms: 20.0,
            out_buffer_ms: 40.0,
            max_ppm: 1000.0,
            blocksize: 480, // dsp::FRAME
            siri_guard: false,
            call_in: "CABLE-A Output".into(),
            siri_mode: "always".into(),
            siri_model: Vec::new(),
            siri_threshold: 0.5,
            siri_profile: DEFAULT_PROFILE.into(),
            siri_depth_db: None,
            siri_lookahead_s: 1.0,
            spk_buffer_ms: 40.0,
            debug_wav: None,
            debug_max_s: 600.0,
            stats_interval_s: 5.0,
        }
    }
}

/// Setting names in declaration order (= order in the saved file).
pub const FIELDS: &[&str] = &[
    "mic",
    "speaker",
    "out",
    "engine",
    "noise_suppression",
    "high_pass",
    "agc",
    "filter_ms",
    "delay_ms",
    "mic_delay_ms",
    "output_gain_db",
    "ref_buffer_ms",
    "out_buffer_ms",
    "max_ppm",
    "blocksize",
    "siri_guard",
    "call_in",
    "siri_mode",
    "siri_model",
    "siri_threshold",
    "siri_profile",
    "siri_depth_db",
    "siri_lookahead_s",
    "spk_buffer_ms",
    "debug_wav",
    "debug_max_s",
    "stats_interval_s",
];

/// `%APPDATA%\desk-echo-guard\echoguard.toml`; `~/.config/...` where there
/// is no APPDATA (like `gui.default_config_path`).
pub fn default_config_path() -> PathBuf {
    let base = match std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        Some(appdata) => PathBuf::from(appdata),
        None => home_dir().join(".config"),
    };
    base.join("desk-echo-guard").join("echoguard.toml")
}

fn home_dir() -> PathBuf {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .next()
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Read a settings file: `[tables]` are flattened one level and `-` in key
/// names becomes `_` (like `cli.load_config`).
pub fn load_config(path: impl AsRef<Path>) -> Result<Table> {
    let path = path.as_ref();
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse_config(&text).with_context(|| format!("invalid TOML in {}", path.display()))
}

/// [`load_config`] on a string.
pub fn parse_config(text: &str) -> Result<Table> {
    let raw: Table = text.parse()?;
    let mut flat = Table::new();
    for (k, v) in raw {
        match v {
            // allow [aec] / [siri] / [devices] tables
            Value::Table(t) => {
                for (k2, v2) in t {
                    flat.insert(k2.replace('-', "_"), v2);
                }
            }
            v => {
                flat.insert(k.replace('-', "_"), v);
            }
        }
    }
    Ok(flat)
}

fn as_str(k: &str, v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        _ => bail!("{k}: expected a string, got {}", v.type_str()),
    }
}

fn as_bool(k: &str, v: &Value) -> Result<bool> {
    match v {
        Value::Boolean(b) => Ok(*b),
        _ => bail!("{k}: expected true or false, got {}", v.type_str()),
    }
}

fn as_f64(k: &str, v: &Value) -> Result<f64> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Integer(i) => Ok(*i as f64),
        _ => bail!("{k}: expected a number, got {}", v.type_str()),
    }
}

fn as_uint(k: &str, v: &Value) -> Result<u64> {
    match v {
        Value::Integer(i) if *i >= 0 => Ok(*i as u64),
        Value::Float(f) if *f >= 0.0 && f.fract() == 0.0 && *f <= u32::MAX as f64 => Ok(*f as u64),
        _ => bail!("{k}: expected a non-negative integer, got {v}"),
    }
}

/// Settings from a flattened table (see [`load_config`]); missing keys take
/// their defaults.  `strict`: an unknown key is an error (CLI `--config`);
/// otherwise unknown keys are ignored (GUI).  `siri_model` may be a string
/// or a list of strings; integers are accepted for float settings.
pub fn settings_from_table(t: &Table, strict: bool) -> Result<Settings> {
    let mut s = Settings::default();
    for (k, v) in t {
        let k = k.as_str();
        match k {
            "mic" => s.mic = Some(as_str(k, v)?),
            "speaker" => s.speaker = Some(as_str(k, v)?),
            "out" => s.out = as_str(k, v)?,
            "engine" => s.engine = as_str(k, v)?,
            "noise_suppression" => s.noise_suppression = as_bool(k, v)?,
            "high_pass" => s.high_pass = as_bool(k, v)?,
            "agc" => s.agc = as_bool(k, v)?,
            "filter_ms" => {
                s.filter_ms = u32::try_from(as_uint(k, v)?).context("filter_ms: too large")?
            }
            "delay_ms" => s.delay_ms = as_f64(k, v)?,
            "mic_delay_ms" => s.mic_delay_ms = as_f64(k, v)?,
            "output_gain_db" => s.output_gain_db = as_f64(k, v)?,
            "ref_buffer_ms" => s.ref_buffer_ms = as_f64(k, v)?,
            "out_buffer_ms" => s.out_buffer_ms = as_f64(k, v)?,
            "max_ppm" => s.max_ppm = as_f64(k, v)?,
            "blocksize" => {
                s.blocksize = usize::try_from(as_uint(k, v)?).context("blocksize: too large")?
            }
            "siri_guard" => s.siri_guard = as_bool(k, v)?,
            "call_in" => s.call_in = as_str(k, v)?,
            "siri_mode" => s.siri_mode = as_str(k, v)?,
            "siri_model" => {
                s.siri_model = match v {
                    Value::String(m) => vec![m.clone()],
                    Value::Array(a) => {
                        a.iter().map(|m| as_str(k, m)).collect::<Result<Vec<_>>>()?
                    }
                    _ => bail!("{k}: expected a string or a list of strings"),
                }
            }
            "siri_threshold" => s.siri_threshold = as_f64(k, v)?,
            "siri_profile" => {
                let p = as_str(k, v)?;
                s.siri_profile = LEGACY_PROFILES
                    .iter()
                    .find(|(old, _)| *old == p)
                    .map_or(p, |(_, new)| (*new).to_string());
            }
            "siri_depth_db" => s.siri_depth_db = Some(as_f64(k, v)?),
            "siri_lookahead_s" => s.siri_lookahead_s = as_f64(k, v)?,
            "spk_buffer_ms" => s.spk_buffer_ms = as_f64(k, v)?,
            "debug_wav" => s.debug_wav = Some(as_str(k, v)?),
            "debug_max_s" => s.debug_max_s = as_f64(k, v)?,
            "stats_interval_s" => s.stats_interval_s = as_f64(k, v)?,
            _ if strict => bail!("unknown key: {k}"),
            _ => {}
        }
    }
    Ok(s)
}

/// TOML value text as `cli._toml_value` writes it.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // Python only escapes \ and "; control characters would make the
            // file unreadable, so escape them too
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Like Python's `repr(float)`: always has a `.` or an exponent.
fn toml_f64(x: f64) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf" } else { "-inf" }.into()
    } else {
        format!("{x:?}")
    }
}

/// The settings file text (header comment, then every non-None setting in
/// declaration order), exactly like `cli.save_config`.
pub fn config_text(s: &Settings) -> String {
    let mut lines =
        vec!["# desk-echo-guard settings (written by the GUI; --config reads it too)".to_string()];
    let b = |v: bool| if v { "true" } else { "false" }.to_string();
    let opt = |v: &Option<String>| v.as_deref().map(toml_str);
    let values: [(&str, Option<String>); 27] = [
        ("mic", opt(&s.mic)),
        ("speaker", opt(&s.speaker)),
        ("out", Some(toml_str(&s.out))),
        ("engine", Some(toml_str(&s.engine))),
        ("noise_suppression", Some(b(s.noise_suppression))),
        ("high_pass", Some(b(s.high_pass))),
        ("agc", Some(b(s.agc))),
        ("filter_ms", Some(s.filter_ms.to_string())),
        ("delay_ms", Some(toml_f64(s.delay_ms))),
        ("mic_delay_ms", Some(toml_f64(s.mic_delay_ms))),
        ("output_gain_db", Some(toml_f64(s.output_gain_db))),
        ("ref_buffer_ms", Some(toml_f64(s.ref_buffer_ms))),
        ("out_buffer_ms", Some(toml_f64(s.out_buffer_ms))),
        ("max_ppm", Some(toml_f64(s.max_ppm))),
        ("blocksize", Some(s.blocksize.to_string())),
        ("siri_guard", Some(b(s.siri_guard))),
        ("call_in", Some(toml_str(&s.call_in))),
        ("siri_mode", Some(toml_str(&s.siri_mode))),
        (
            "siri_model",
            Some(format!(
                "[{}]",
                s.siri_model
                    .iter()
                    .map(|m| toml_str(m))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        ),
        ("siri_threshold", Some(toml_f64(s.siri_threshold))),
        ("siri_profile", Some(toml_str(&s.siri_profile))),
        ("siri_depth_db", s.siri_depth_db.map(toml_f64)),
        ("siri_lookahead_s", Some(toml_f64(s.siri_lookahead_s))),
        ("spk_buffer_ms", Some(toml_f64(s.spk_buffer_ms))),
        ("debug_wav", opt(&s.debug_wav)),
        ("debug_max_s", Some(toml_f64(s.debug_max_s))),
        ("stats_interval_s", Some(toml_f64(s.stats_interval_s))),
    ];
    debug_assert!(values.iter().map(|(k, _)| *k).eq(FIELDS.iter().copied()));
    for (k, v) in values {
        if let Some(v) = v {
            lines.push(format!("{k} = {v}"));
        }
    }
    lines.join("\n") + "\n"
}

/// Write every non-None setting as a flat TOML file (readable by
/// [`load_config`] and by the Python version); creates the directory.
pub fn save_config(path: impl AsRef<Path>, s: &Settings) -> Result<()> {
    let path = path.as_ref();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(path, config_text(s)).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("echoguard-core-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn config_file_tables_dashes_and_string_model() {
        // tests/test_cli.py::test_config_file_and_cli_override (file part)
        let t = parse_config(
            "mic = \"USB\"\ndelay-ms = 40\n[siri]\nsiri_mode = \"always\"\n\
             siri_model = \"x.onnx\"\n",
        )
        .unwrap();
        let s = settings_from_table(&t, true).unwrap();
        assert_eq!(s.mic.as_deref(), Some("USB"));
        assert_eq!(s.delay_ms, 40.0);
        assert_eq!(s.siri_mode, "always");
        assert_eq!(s.siri_model, vec!["x.onnx".to_string()]);
        assert_eq!(s.out, "CABLE Input");
    }

    #[test]
    fn unknown_keys_strict_vs_lenient() {
        let t = parse_config("mic = \"USB\"\nbogus = 1\n").unwrap();
        let e = settings_from_table(&t, true).unwrap_err();
        assert!(e.to_string().contains("bogus"));
        let s = settings_from_table(&t, false).unwrap();
        assert_eq!(s.mic.as_deref(), Some("USB"));
    }

    #[test]
    fn types_and_legacy_profile() {
        let t = parse_config(
            "siri_model = [\"a.onnx\", \"b.onnx\"]\nsiri_profile = \"apple\"\n\
             filter_ms = 200.0\nsiri_depth_db = -40\nmax_ppm = 500\n",
        )
        .unwrap();
        let s = settings_from_table(&t, true).unwrap();
        assert_eq!(s.siri_model, vec!["a.onnx", "b.onnx"]);
        assert_eq!(s.siri_profile, "deep-wide");
        assert_eq!(s.filter_ms, 200);
        assert_eq!(s.siri_depth_db, Some(-40.0));
        assert_eq!(s.max_ppm, 500.0);
        for bad in [
            "agc = 1",
            "mic = 3",
            "filter_ms = -1",
            "blocksize = 1.5",
            "siri_model = 2",
        ] {
            let t = parse_config(bad).unwrap();
            assert!(settings_from_table(&t, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn defaults_text_matches_python() {
        // what `cli.save_config(path, Settings())` writes
        let expected = "# desk-echo-guard settings (written by the GUI; --config reads it too)\n\
out = \"CABLE Input\"\nengine = \"auto\"\nnoise_suppression = true\nhigh_pass = true\n\
agc = false\nfilter_ms = 300\ndelay_ms = 0.0\nmic_delay_ms = 0.0\noutput_gain_db = 0.0\n\
ref_buffer_ms = 20.0\nout_buffer_ms = 40.0\nmax_ppm = 1000.0\nblocksize = 480\n\
siri_guard = false\ncall_in = \"CABLE-A Output\"\nsiri_mode = \"always\"\nsiri_model = []\n\
siri_threshold = 0.5\nsiri_profile = \"deep-wide\"\nsiri_lookahead_s = 1.0\n\
spk_buffer_ms = 40.0\ndebug_max_s = 600.0\nstats_interval_s = 5.0\n";
        assert_eq!(config_text(&Settings::default()), expected);
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tmp("roundtrip");
        let path = dir.join("sub").join("echoguard.toml");
        let s = Settings {
            mic: Some("USB \"Pro\" \\ 麥克風\ttab".into()),
            speaker: Some("Speakers (Realtek(R) Audio)".into()),
            out: "回聲消除麥克風".into(),
            engine: "webrtc".into(),
            agc: true,
            filter_ms: 123,
            delay_ms: 12.5,
            mic_delay_ms: -3.0,
            output_gain_db: 1e-7,
            max_ppm: 1e20,
            blocksize: 960,
            siri_guard: true,
            siri_mode: "detect".into(),
            siri_model: vec!["C:\\models\\hey_siri.onnx".into(), "b.onnx".into()],
            siri_threshold: 0.35,
            siri_depth_db: Some(-45.0),
            debug_wav: Some("C:\\tmp\\wav".into()),
            ..Settings::default()
        };
        save_config(&path, &s).unwrap();
        let back = settings_from_table(&load_config(&path).unwrap(), true).unwrap();
        assert_eq!(back, s);
        let d = Settings::default();
        save_config(&path, &d).unwrap();
        assert_eq!(
            settings_from_table(&load_config(&path).unwrap(), true).unwrap(),
            d
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_repo_example_config() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../echoguard.example.toml");
        let s = settings_from_table(&load_config(&path).unwrap(), true).unwrap();
        assert_eq!(s.mic.as_deref(), Some("USB"));
        assert_eq!(s.speaker.as_deref(), Some("Realtek"));
        assert_eq!(s.engine, "webrtc");
        assert_eq!(s.delay_ms, 0.0);
        assert!(!s.siri_guard);
        assert_eq!(s.call_in, "CABLE-A Output");
        assert_eq!(s.siri_profile, "deep-wide");
        assert!(s.siri_model.is_empty());
    }

    #[test]
    fn serde_defaults_for_missing_fields() {
        let s: Settings = toml::from_str("mic = \"USB\"").unwrap();
        assert_eq!(s.mic.as_deref(), Some("USB"));
        assert_eq!(s.filter_ms, 300);
    }

    #[test]
    fn default_path_ends_with_app_dir() {
        let p = default_config_path();
        assert!(p.ends_with("desk-echo-guard/echoguard.toml"));
    }
}
