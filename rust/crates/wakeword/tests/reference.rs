//! Compare the Rust pipeline with scores produced by the Python openWakeWord
//! package (tests/fixtures/gen_reference.py).  Needs the openWakeWord ONNX
//! models: they are downloaded into `$ECHOGUARD_OWW_MODELS` (default: a
//! folder in the cargo target dir); the tests are skipped when that fails or
//! `ECHOGUARD_OWW_OFFLINE` is set.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use echoguard_dsp::IntegerResampler;
use echoguard_wakeword::{download, feature_model_paths, OpenWakeWordDetector, OwwModel, HOP, RELEASE_URL};

const FILES: [&str; 4] = [
    "melspectrogram.onnx",
    "embedding_model.onnx",
    "hey_mycroft_v0.1.onnx",
    "alexa_v0.1.onnx",
];
const TOL: f32 = 1e-3;

fn models_dir() -> Option<PathBuf> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = std::env::var_os("ECHOGUARD_OWW_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("oww-models"));
    for f in FILES {
        let p = dir.join(f);
        if p.is_file() {
            continue;
        }
        if std::env::var_os("ECHOGUARD_OWW_OFFLINE").is_some() {
            eprintln!("SKIP: {} missing and ECHOGUARD_OWW_OFFLINE is set", p.display());
            return None;
        }
        if let Err(e) = download(&format!("{RELEASE_URL}/{f}"), &p) {
            eprintln!("SKIP: cannot download {f}: {e:#}");
            return None;
        }
    }
    Some(dir)
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn clip() -> Vec<i16> {
    std::fs::read(fixture("clip16k.i16"))
        .expect("fixture")
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

fn rows(name: &str) -> Vec<Vec<f64>> {
    std::fs::read_to_string(fixture(name))
        .expect("fixture")
        .lines()
        .map(|l| l.split_whitespace().map(|v| v.parse().expect("number")).collect())
        .collect()
}

fn model(dir: &Path) -> OwwModel {
    OwwModel::new(
        &[dir.join(FILES[2]), dir.join(FILES[3])],
        &dir.join(FILES[0]),
        &dir.join(FILES[1]),
    )
    .expect("load models")
}

#[test]
fn scores_match_python_per_hop() {
    let Some(dir) = models_dir() else { return };
    let mut m = model(&dir);
    assert_eq!(m.labels(), ["hey_mycroft_v0.1", "alexa_v0.1"]);
    let refs = rows("model_scores_1280.txt");
    let x = clip();
    assert_eq!(refs.len(), x.len() / HOP);
    let mut worst = 0.0f32;
    let t0 = Instant::now();
    for (i, (chunk, r)) in x.chunks(HOP).zip(&refs).enumerate() {
        let s = m.predict(chunk).expect("predict");
        for (a, b) in s.iter().zip(r) {
            let d = (a - *b as f32).abs();
            worst = worst.max(d);
            assert!(d < TOL, "hop {i}: rust {s:?} python {r:?}");
        }
        if i < 5 {
            assert!(s.iter().all(|&v| v == 0.0), "first 5 predictions are zeroed");
        }
    }
    let per_hop = t0.elapsed().as_secs_f64() * 1e3 / refs.len() as f64;
    eprintln!("max |rust - python| = {worst:.2e}; {per_hop:.3} ms per 80 ms hop (2 wake-word models)");
    // the clip contains both phrases
    assert!(refs.iter().any(|r| r[0] > 0.9) && refs.iter().any(|r| r[1] > 0.9));
}

#[test]
fn irregular_chunks_match_python() {
    let Some(dir) = models_dir() else { return };
    let mut m = model(&dir);
    let x = clip();
    let mut pos = 0;
    let mut worst = 0.0f32;
    for (i, r) in rows("model_scores_irregular.txt").iter().enumerate() {
        let n = r[0] as usize;
        let s = m.predict(&x[pos..pos + n]).expect("predict");
        pos += n;
        for (a, b) in s.iter().zip(&r[1..]) {
            let d = (a - *b as f32).abs();
            worst = worst.max(d);
            assert!(d < TOL, "call {i} (n={n}): rust {s:?} python {r:?}");
        }
    }
    assert_eq!(pos, x.len());
    eprintln!("irregular chunks: max |rust - python| = {worst:.2e}");
}

#[test]
fn detector_matches_python() {
    let Some(dir) = models_dir() else { return };
    let x: Vec<f32> = clip().iter().map(|&v| v as f32 / 32768.0).collect();
    let up = IntegerResampler::new(-3).process(&x);
    let mut det = OpenWakeWordDetector::with_feature_models(
        &[dir.join(FILES[2])],
        &dir.join(FILES[0]),
        &dir.join(FILES[1]),
        0.5,
        0.9,
        0.15,
        1.0,
    )
    .expect("detector");
    let mut hops = Vec::new();
    let mut regions = Vec::new();
    for (b, blk) in up.chunks_exact(480).enumerate() {
        let start = (b * 480) as i64;
        regions.extend(det.feed_with(blk, start, |t, s| hops.push((start, t, s))));
    }
    assert_eq!(det.errors(), 0);
    let text = std::fs::read_to_string(fixture("detector_ref.txt")).expect("fixture");
    let mut ref_hops = Vec::new();
    let mut ref_regions = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        match f[0] {
            "hop" => ref_hops.push((f[1].parse::<i64>().unwrap(), f[2].parse::<f32>().unwrap())),
            "region" => ref_regions.push((f[1].parse::<i64>().unwrap(), f[2].parse::<i64>().unwrap())),
            _ => panic!("bad fixture line {l}"),
        }
    }
    assert_eq!(hops.len(), ref_hops.len());
    let mut worst = 0.0f32;
    for (&(start, t, s), &(rs, rscore)) in hops.iter().zip(&ref_hops) {
        assert_eq!(start, rs, "hop completes in the same block");
        assert_eq!(t, start + 480, "480-sample blocks: t = block end");
        worst = worst.max((s - rscore).abs());
        assert!((s - rscore).abs() < TOL, "block {start}: rust {s} python {rscore}");
    }
    assert_eq!(regions, ref_regions);
    eprintln!("detector: max |rust - python| = {worst:.2e}, regions {regions:?}");
}

#[test]
fn feature_models_found_next_to_model() {
    let Some(dir) = models_dir() else { return };
    let (m, e) = feature_model_paths(&[dir.join(FILES[2])]).expect("paths");
    assert_eq!(m, std::path::absolute(dir.join(FILES[0])).unwrap());
    assert_eq!(e, std::path::absolute(dir.join(FILES[1])).unwrap());
}
