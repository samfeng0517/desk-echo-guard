//! Command line interface, identical to `echoguard/cli.py` (same options,
//! defaults, choices and messages), plus the exe-only entries `--vcable`
//! (UAC helper), `--self-test`, `--autostart` and `--screenshot`.
//!
//! ```text
//! DeskEchoGuard --list-devices
//! DeskEchoGuard --mic "USB" --speaker "Realtek" --out "CABLE Input"
//! DeskEchoGuard --config echoguard.toml --siri-guard --siri-mode always
//! DeskEchoGuard --calibrate --mic "USB" --speaker "Realtek"
//! DeskEchoGuard --sim-mic mic.wav --sim-ref ref.wav --sim-out out.wav
//! DeskEchoGuard --sim-guard-in call.wav --sim-guard-out call_guarded.wav --siri-mode always
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{ArgAction, Parser};
use echoguard_aec::{available_engines, make_engine, AecProcessor, EngineOptions};
use echoguard_audio::DeviceKind;
use echoguard_core::settings::FIELDS;
use echoguard_core::vcable::{self, CableAction};
use echoguard_core::{load_config, settings_from_table, LogSink, Settings};
use echoguard_dsp::FRAME;

use crate::wav::{read_wav, write_wav};

/// `cli._bool`: "1", "true", "yes", "on" (any case) are true, anything else
/// is false.
fn parse_bool(v: &str) -> Result<bool, String> {
    Ok(matches!(
        v.to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    ))
}

#[derive(Parser, Clone, Debug, Default, PartialEq)]
#[command(
    name = "DeskEchoGuard",
    about = "System-wide echo cancellation (speaker loopback reference -> VB-Cable virtual \
             mic) + optional 'Hey Siri' anti-trigger filter.",
    disable_version_flag = true,
    infer_long_args = true
)]
pub struct Cli {
    /// show program's version number and exit
    #[arg(long)]
    pub version: bool,
    /// TOML config file (keys = long option names, with - or _)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// list audio devices and exit
    #[arg(long)]
    pub list_devices: bool,
    /// list available AEC engines
    #[arg(long)]
    pub list_engines: bool,
    /// rename: give the VB-Cable devices descriptive names; hide: disable the
    /// VB-Cable devices this program does not use; restore: undo both. Run
    /// from an administrator prompt
    #[arg(long, value_parser = ["rename", "hide", "restore"])]
    pub rename_vbcable: Option<String>,
    /// play noise bursts, measure speaker->mic delay, print a --delay-ms hint
    #[arg(long)]
    pub calibrate: bool,

    // ---- devices
    /// physical microphone (default: system default)
    #[arg(long, help_heading = "devices (case-insensitive name substring)")]
    pub mic: Option<String>,
    /// real speakers: loopback reference and Siri-guard output (default:
    /// system default output)
    #[arg(long, help_heading = "devices (case-insensitive name substring)")]
    pub speaker: Option<String>,
    /// virtual mic sink (default "CABLE Input")
    #[arg(long, help_heading = "devices (case-insensitive name substring)")]
    pub out: Option<String>,
    /// --siri-guard: where Discord/LINE plays to (default "CABLE-A Output")
    #[arg(long, help_heading = "devices (case-insensitive name substring)")]
    pub call_in: Option<String>,

    // ---- echo cancellation
    #[arg(long, help_heading = "echo cancellation",
          value_parser = ["auto", "webrtc", "speex", "nlms", "off"])]
    pub engine: Option<String>,
    /// extra delay applied to the reference (ms); see --calibrate
    #[arg(long, help_heading = "echo cancellation", allow_negative_numbers = true)]
    pub delay_ms: Option<f64>,
    /// extra delay applied to the mic, if the reference arrives late
    #[arg(long, help_heading = "echo cancellation", allow_negative_numbers = true)]
    pub mic_delay_ms: Option<f64>,
    #[arg(long, help_heading = "echo cancellation", value_name = "on|off",
          value_parser = parse_bool)]
    pub noise_suppression: Option<bool>,
    #[arg(long, help_heading = "echo cancellation", value_name = "on|off",
          value_parser = parse_bool)]
    pub high_pass: Option<bool>,
    #[arg(long, help_heading = "echo cancellation", value_name = "on|off",
          value_parser = parse_bool)]
    pub agc: Option<bool>,
    /// speex/nlms filter length
    #[arg(long, help_heading = "echo cancellation")]
    pub filter_ms: Option<u32>,
    #[arg(long, help_heading = "echo cancellation", allow_negative_numbers = true)]
    pub output_gain_db: Option<f64>,
    #[arg(long, help_heading = "echo cancellation")]
    pub ref_buffer_ms: Option<f64>,
    #[arg(long, help_heading = "echo cancellation")]
    pub out_buffer_ms: Option<f64>,
    /// max clock-drift correction
    #[arg(long, help_heading = "echo cancellation")]
    pub max_ppm: Option<f64>,
    /// device block size (samples)
    #[arg(long, help_heading = "echo cancellation")]
    pub blocksize: Option<usize>,

    // ---- Hey Siri anti-trigger
    /// enable pipeline B (call audio -> filter -> speakers)
    #[arg(long, help_heading = "Hey Siri anti-trigger")]
    pub siri_guard: bool,
    #[arg(long, help_heading = "Hey Siri anti-trigger",
          value_parser = ["detect", "sibilant", "always"])]
    pub siri_mode: Option<String>,
    /// openWakeWord model (.onnx/.tflite) for --siri-mode detect
    #[arg(long, help_heading = "Hey Siri anti-trigger", action = ArgAction::Append)]
    pub siri_model: Vec<String>,
    #[arg(long, help_heading = "Hey Siri anti-trigger")]
    pub siri_threshold: Option<f64>,
    #[arg(long, help_heading = "Hey Siri anti-trigger",
          value_parser = ["deep-wide", "apple-deep", "apple-wide", "formant"])]
    pub siri_profile: Option<String>,
    #[arg(long, help_heading = "Hey Siri anti-trigger", allow_negative_numbers = true)]
    pub siri_depth_db: Option<f64>,
    #[arg(long, help_heading = "Hey Siri anti-trigger")]
    pub siri_lookahead_s: Option<f64>,

    // ---- debug / offline
    /// directory to dump mic/ref/out wavs on exit
    #[arg(long, help_heading = "debug / offline")]
    pub debug_wav: Option<String>,
    #[arg(long, help_heading = "debug / offline")]
    pub stats_interval_s: Option<f64>,
    /// stop after N seconds
    #[arg(long, help_heading = "debug / offline")]
    pub duration_s: Option<f64>,
    /// offline AEC: mic recording (wav)
    #[arg(long, help_heading = "debug / offline")]
    pub sim_mic: Option<PathBuf>,
    /// offline AEC: reference recording (wav)
    #[arg(long, help_heading = "debug / offline")]
    pub sim_ref: Option<PathBuf>,
    /// offline AEC: output wav
    #[arg(long, help_heading = "debug / offline")]
    pub sim_out: Option<PathBuf>,
    /// offline Siri guard: input wav
    #[arg(long, help_heading = "debug / offline")]
    pub sim_guard_in: Option<PathBuf>,
    /// offline Siri guard: output wav
    #[arg(long, help_heading = "debug / offline")]
    pub sim_guard_out: Option<PathBuf>,

    // ---- this exe only
    /// GUI: launched at sign-in (stay in the tray and start right away)
    #[arg(long, help_heading = "program")]
    pub autostart: bool,
    /// run the self-test, print it (and write REPORT); exit code 0 = pass
    #[arg(long, help_heading = "program", value_name = "REPORT",
          num_args = 0..=1)]
    pub self_test: Option<Option<PathBuf>>,
    /// internal: UAC-elevated VB-Cable helper
    #[arg(long, hide = true, num_args = 2, value_names = ["ACTIONS", "RESULT"])]
    pub vcable: Option<Vec<String>>,
    /// development: render every GUI page to PNG files in DIR
    #[arg(long, hide = true, value_name = "DIR")]
    pub screenshot: Option<PathBuf>,
}

/// What the exe does with these arguments.
#[derive(Debug, PartialEq)]
pub enum Mode {
    Gui {
        config: Option<PathBuf>,
        autostart: bool,
    },
    Screenshot(PathBuf),
    Version,
    /// `--vcable ACTIONS RESULT.json` (arguments after `--vcable`)
    Vcable(Vec<String>),
    SelfTest(Option<PathBuf>),
    /// any `cli.py` action or option
    Console,
}

impl Cli {
    pub fn mode(&self) -> Mode {
        if self.version {
            return Mode::Version;
        }
        if let Some(a) = &self.vcable {
            return Mode::Vcable(a.clone());
        }
        if let Some(r) = &self.self_test {
            return Mode::SelfTest(r.clone());
        }
        if let Some(d) = &self.screenshot {
            return Mode::Screenshot(d.clone());
        }
        // only --config / --autostart (or nothing): the GUI
        let gui_only = Cli {
            config: None,
            autostart: false,
            ..self.clone()
        };
        if gui_only == Cli::default() {
            Mode::Gui {
                config: self.config.clone(),
                autostart: self.autostart,
            }
        } else {
            Mode::Console
        }
    }
}

/// `cli.make_settings`: the config file (unknown keys are an error), then
/// every option given on the command line on top.
pub fn make_settings(cli: &Cli) -> Result<Settings> {
    let mut s = match &cli.config {
        Some(path) => {
            let t = load_config(path)?;
            if let Some(k) = t.keys().find(|k| !FIELDS.contains(&k.as_str())) {
                bail!("unknown key in {}: {k}", path.display());
            }
            settings_from_table(&t, true).with_context(|| format!("{}", path.display()))?
        }
        None => Settings::default(),
    };
    macro_rules! set {
        ($($f:ident),*) => {$( if let Some(v) = &cli.$f { s.$f = v.clone(); } )*};
    }
    macro_rules! set_opt {
        ($($f:ident),*) => {$( if let Some(v) = &cli.$f { s.$f = Some(v.clone()); } )*};
    }
    set!(out, call_in, engine, delay_ms, mic_delay_ms, noise_suppression, high_pass, agc);
    set!(filter_ms, output_gain_db, ref_buffer_ms, out_buffer_ms, max_ppm, blocksize);
    set!(siri_mode, siri_threshold, siri_profile, siri_lookahead_s, stats_interval_s);
    set_opt!(mic, speaker, siri_depth_db, debug_wav);
    if cli.siri_guard {
        s.siri_guard = true;
    }
    if !cli.siri_model.is_empty() {
        s.siri_model = cli.siri_model.clone();
    }
    Ok(s)
}

fn print_log() -> LogSink {
    Arc::new(|line: String| println!("{line}"))
}

/// Console mode (`cli.main`); returns the exit code.  Errors are printed by
/// the caller (exit code 1, like Python's `SystemExit("message")`).
pub fn run(cli: &Cli) -> Result<i32> {
    let st = make_settings(cli)?;

    if cli.list_engines {
        println!("available AEC engines: {}", available_engines().join(", "));
        return Ok(0);
    }

    if cli.sim_mic.is_some() || cli.sim_ref.is_some() {
        let (Some(mic), Some(reference), Some(out)) = (&cli.sim_mic, &cli.sim_ref, &cli.sim_out)
        else {
            bail!("--sim-mic, --sim-ref and --sim-out go together");
        };
        let (mic, reference) = (read_wav(mic)?, read_wav(reference)?);
        let eng = make_engine(
            &st.engine,
            &EngineOptions {
                noise_suppression: st.noise_suppression,
                high_pass_filter: st.high_pass,
                auto_gain_control: st.agc,
                filter_ms: st.filter_ms,
            },
        )?;
        let mut proc = AecProcessor::new(eng, st.delay_ms, st.mic_delay_ms, st.output_gain_db);
        write_wav(out, &proc.process_signal(&mic, &reference)?)?;
        println!(
            "[sim] engine={} wrote {}",
            proc.engine_name(),
            out.display()
        );
        return Ok(0);
    }

    if let Some(input) = &cli.sim_guard_in {
        let Some(out) = &cli.sim_guard_out else {
            bail!("--sim-guard-in needs --sim-guard-out");
        };
        let mut g = echoguard_core::make_guard(&st)?;
        write_wav(out, &g.process_signal(&read_wav(input)?, FRAME))?;
        println!(
            "[sim] siri guard mode={}: {} region(s) processed; wrote {}",
            st.siri_mode,
            g.detections(),
            out.display()
        );
        return Ok(0);
    }

    if let Some(action) = &cli.rename_vbcable {
        let action: CableAction = action.parse()?;
        let done = vcable::apply(&[action])?;
        if done.is_empty() {
            println!("nothing to change");
        }
        for line in done {
            println!("{line}");
        }
        return Ok(0);
    }

    let backend = echoguard_audio::default_backend()?;

    if cli.list_devices {
        let devs = backend.list_devices()?;
        for (kind, title) in [
            (DeviceKind::Mic, "Microphones (--mic, --call-in)"),
            (
                DeviceKind::Speaker,
                "Speakers / outputs (--speaker, --out; loopback works on these)",
            ),
        ] {
            println!("{title}:");
            for d in devs.iter().filter(|d| d.kind == kind) {
                let star = if d.default { '*' } else { ' ' };
                println!("  {star} {}  [{} ch]", d.name, d.channels);
            }
        }
        println!("(* = system default)");
        return Ok(0);
    }

    if cli.calibrate {
        let (d, conf) = echoguard_core::calibrate(&st, backend, 4.0, -20.0, print_log())?;
        if conf < 5.0 {
            println!("low confidence: raise speaker volume / move mic closer and retry");
        } else if d >= 0.0 {
            println!(
                "suggestion: webrtc needs nothing (AEC3 estimates delay itself); for \
                 speex/nlms with a long delay use --delay-ms {:.0}",
                (d - 20.0).max(0.0)
            );
        } else {
            println!(
                "reference arrives late: use --mic-delay-ms {:.0}",
                -d + 10.0
            );
        }
        return Ok(0);
    }

    let devs = backend.list_devices()?;
    println!(
        "desk-echo-guard {}: mic={} speaker={} -> {}.  In Discord/LINE choose '{}' as the \
         microphone.  Ctrl+C stops.",
        env!("CARGO_PKG_VERSION"),
        st.mic.as_deref().unwrap_or("(default)"),
        st.speaker.as_deref().unwrap_or("(default)"),
        st.out,
        vcable::display_name(&devs, "virtual_mic")
    );
    if st.siri_guard {
        println!(
            "siri guard: set Discord/LINE OUTPUT device to '{}'.",
            vcable::display_name(&devs, "call_speaker")
        );
    }
    echoguard_core::run_blocking(&st, backend, cli.duration_s, print_log())?;
    Ok(0)
}

/// `--self-test [report]`.
pub fn self_test(report: Option<&Path>) -> i32 {
    echoguard_core::selftest::main(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("DeskEchoGuard").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("deg-cli-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn config_file_and_cli_override() {
        // tests/test_cli.py::test_config_file_and_cli_override
        let cfg = tmp("c.toml");
        std::fs::write(
            &cfg,
            "mic = \"USB\"\ndelay-ms = 40\n[siri]\nsiri_mode = \"always\"\n\
             siri_model = \"x.onnx\"\n",
        )
        .unwrap();
        let cli = parse(&["--config", cfg.to_str().unwrap(), "--delay-ms", "12"]);
        let st = make_settings(&cli).unwrap();
        assert_eq!(st.mic.as_deref(), Some("USB"));
        assert_eq!(st.delay_ms, 12.0);
        assert_eq!(st.siri_mode, "always");
        assert_eq!(st.siri_model, vec!["x.onnx".to_string()]);
        assert_eq!(st.out, "CABLE Input");
    }

    #[test]
    fn unknown_config_key_is_an_error() {
        let cfg = tmp("bad.toml");
        std::fs::write(&cfg, "mic = \"USB\"\nmikrofon = 1\n").unwrap();
        let err = make_settings(&parse(&["--config", cfg.to_str().unwrap()])).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            format!("unknown key in {}: mikrofon", cfg.display())
        );
    }

    #[test]
    fn defaults_without_options() {
        assert_eq!(make_settings(&parse(&[])).unwrap(), Settings::default());
    }

    #[test]
    fn every_option_maps_to_its_setting() {
        let st = make_settings(&parse(&[
            "--mic", "m", "--speaker", "s", "--out", "o", "--call-in", "c",
            "--engine", "speex", "--delay-ms", "-5", "--mic-delay-ms", "7",
            "--noise-suppression", "off", "--high-pass", "No", "--agc", "ON",
            "--filter-ms", "200", "--output-gain-db", "-3", "--ref-buffer-ms", "30",
            "--out-buffer-ms", "50", "--max-ppm", "500", "--blocksize", "960",
            "--siri-guard", "--siri-mode", "sibilant", "--siri-model", "a.onnx",
            "--siri-model", "b.onnx", "--siri-threshold", "0.7", "--siri-profile",
            "formant", "--siri-depth-db", "-30", "--siri-lookahead-s", "0.5",
            "--debug-wav", "dbg", "--stats-interval-s", "0",
        ]))
        .unwrap();
        let want = Settings {
            mic: Some("m".into()),
            speaker: Some("s".into()),
            out: "o".into(),
            call_in: "c".into(),
            engine: "speex".into(),
            delay_ms: -5.0,
            mic_delay_ms: 7.0,
            noise_suppression: false,
            high_pass: false,
            agc: true,
            filter_ms: 200,
            output_gain_db: -3.0,
            ref_buffer_ms: 30.0,
            out_buffer_ms: 50.0,
            max_ppm: 500.0,
            blocksize: 960,
            siri_guard: true,
            siri_mode: "sibilant".into(),
            siri_model: vec!["a.onnx".into(), "b.onnx".into()],
            siri_threshold: 0.7,
            siri_profile: "formant".into(),
            siri_depth_db: Some(-30.0),
            siri_lookahead_s: 0.5,
            debug_wav: Some("dbg".into()),
            stats_interval_s: 0.0,
            ..Default::default()
        };
        assert_eq!(st, want);
    }

    #[test]
    fn bool_values_like_python() {
        for (v, want) in [("1", true), ("true", true), ("Yes", true), ("on", true)] {
            assert_eq!(parse_bool(v), Ok(want), "{v}");
        }
        for v in ["0", "off", "false", "no", "whatever"] {
            assert_eq!(parse_bool(v), Ok(false), "{v}");
        }
    }

    #[test]
    fn choices_are_checked() {
        for args in [
            &["--engine", "foo"][..],
            &["--siri-mode", "never"],
            &["--siri-profile", "apple"],
            &["--rename-vbcable", "delete"],
            &["--filter-ms", "x"],
            &["--no-such-option"],
        ] {
            assert!(
                Cli::try_parse_from(std::iter::once("x").chain(args.iter().copied())).is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn abbreviated_long_options() {
        // argparse accepts unambiguous prefixes
        assert!(parse(&["--list-dev"]).list_devices);
        assert_eq!(parse(&["--dur", "3"]).duration_s, Some(3.0));
    }

    #[test]
    fn dispatch() {
        let gui = |c: Option<&str>, a| Mode::Gui {
            config: c.map(PathBuf::from),
            autostart: a,
        };
        assert_eq!(parse(&[]).mode(), gui(None, false));
        assert_eq!(parse(&["--autostart"]).mode(), gui(None, true));
        assert_eq!(
            parse(&["--config", "x.toml", "--autostart"]).mode(),
            gui(Some("x.toml"), true)
        );
        for args in [
            &["--list-devices"][..],
            &["--list-engines"],
            &["--calibrate"],
            &["--rename-vbcable", "hide"],
            &["--mic", "USB"],
            &["--config", "x.toml", "--engine", "nlms"],
            &["--siri-guard"],
            &["--siri-model", "a.onnx"],
            &["--duration-s", "5"],
            &["--sim-guard-in", "a.wav", "--sim-guard-out", "b.wav"],
        ] {
            assert_eq!(parse(args).mode(), Mode::Console, "{args:?}");
        }
        assert_eq!(parse(&["--version"]).mode(), Mode::Version);
        assert_eq!(
            parse(&["--vcable", "rename,hide", "r.json"]).mode(),
            Mode::Vcable(vec!["rename,hide".into(), "r.json".into()])
        );
        assert_eq!(parse(&["--self-test"]).mode(), Mode::SelfTest(None));
        assert_eq!(
            parse(&["--self-test", "r.txt"]).mode(),
            Mode::SelfTest(Some("r.txt".into()))
        );
        assert_eq!(
            parse(&["--screenshot", "d"]).mode(),
            Mode::Screenshot("d".into())
        );
    }

    #[test]
    fn sim_options_go_together() {
        let err = run(&parse(&["--sim-mic", "a.wav"])).unwrap_err();
        assert_eq!(err.to_string(), "--sim-mic, --sim-ref and --sim-out go together");
        let err = run(&parse(&["--sim-guard-in", "a.wav"])).unwrap_err();
        assert_eq!(err.to_string(), "--sim-guard-in needs --sim-guard-out");
    }
}
