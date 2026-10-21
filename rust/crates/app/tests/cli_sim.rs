//! The offline `--sim-*` commands end to end through the real exe
//! (`tests/test_cli.py::test_offline_sim_commands`), plus console-mode
//! smoke tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use echoguard_dsp::{erle_db, make_scenario};

const EXE: &str = env!("CARGO_BIN_EXE_DeskEchoGuard");

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("deg-it-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(path: &Path, x: &[f32]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &v in x {
        w.write_sample((v * 32767.0).clamp(-32768.0, 32767.0) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
}

fn read(path: &Path) -> Vec<f32> {
    hound::WavReader::open(path)
        .unwrap()
        .samples::<i16>()
        .map(|v| v.unwrap() as f32 / 32768.0)
        .collect()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let o = Command::new(EXE).args(args).output().expect("run exe");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn offline_sim_commands() {
    let d = tmpdir("sim");
    let sc = make_scenario(12.0, 40.0, 0);
    let (mic, rf, out, g) = (
        d.join("mic.wav"),
        d.join("ref.wav"),
        d.join("o.wav"),
        d.join("g.wav"),
    );
    write(&mic, &sc.mic);
    write(&rf, &sc.reference);
    let s = |p: &PathBuf| p.to_str().unwrap().to_string();
    let (code, stdout, stderr) = run(&[
        "--engine",
        "nlms",
        "--sim-mic",
        &s(&mic),
        "--sim-ref",
        &s(&rf),
        "--sim-out",
        &s(&out),
    ]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("[sim] engine=nlms wrote"), "{stdout}");
    let o = read(&out);
    let n = o.len();
    assert!(n > 11 * 48_000);
    let erle = erle_db(&sc.mic[..n], &o, Some(&sc.far_only[..n]), 3.0);
    assert!(erle > 8.0, "ERLE {erle:.1} dB");

    let (code, stdout, stderr) = run(&[
        "--siri-mode",
        "always",
        "--sim-guard-in",
        &s(&rf),
        "--sim-guard-out",
        &s(&g),
    ]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("[sim] siri guard mode=always"), "{stdout}");
    assert_eq!(read(&g).len(), sc.reference.len());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn console_smoke() {
    let (code, stdout, _) = run(&["--list-engines"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("available AEC engines: "), "{stdout}");
    assert!(stdout.contains("nlms"));

    let (code, stdout, _) = run(&["--version"]);
    assert_eq!((code, stdout.trim()), (0, env!("CARGO_PKG_VERSION")));

    let (code, _, stderr) = run(&["--engine", "bogus"]);
    assert_eq!(code, 2, "{stderr}");

    let (code, _, stderr) = run(&["--sim-mic", "x.wav"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("--sim-mic, --sim-ref and --sim-out go together"));

    let d = tmpdir("cfg");
    let cfg = d.join("bad.toml");
    std::fs::write(&cfg, "nope = 1\n").unwrap();
    let (code, _, stderr) = run(&["--config", cfg.to_str().unwrap(), "--list-engines"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown key in"), "{stderr}");
    let _ = std::fs::remove_dir_all(d);
}
