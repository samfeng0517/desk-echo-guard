//! The `heysiri` guard mode on recorded material (16 kHz clips in
//! `fixtures/heysiri`, see the README there for their sources).

use echoguard_dsp::{
    band_energy_db, Detector, GuardConfig, GuardMode, HeySiriDetector, IntegerResampler, SiriGuard,
    FS,
};

/// A 16 kHz s16le fixture, upsampled to 48 kHz, with 0.5 s of silence
/// before and after.
fn clip(name: &str) -> Vec<f32> {
    let path = format!(
        "{}/tests/fixtures/heysiri/{name}.i16",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read(path).unwrap();
    let x: Vec<f32> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
        .collect();
    let mut rs = IntegerResampler::new(-3);
    let mut y = vec![0.0; FS / 2];
    y.extend(rs.process(&x));
    y.extend(rs.process(&vec![0.0; FS / 6]));
    y
}

fn regions(det: &mut dyn Detector, x: &[f32], block: usize) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    for (k, c) in x.chunks(block).enumerate() {
        out.extend(det.feed(c, (k * block) as i64));
    }
    out
}

/// Total length (seconds) of the union of `regions`.
fn covered(regions: &[(i64, i64)]) -> f64 {
    let mut r = regions.to_vec();
    r.sort_unstable();
    let (mut total, mut end) = (0, i64::MIN);
    for (a, b) in r {
        let a = a.max(end);
        if b > a {
            total += b - a;
            end = b;
        }
    }
    total as f64 / FS as f64
}

fn guard(det: HeySiriDetector) -> SiriGuard {
    let cfg = GuardConfig {
        mode: GuardMode::HeySiri,
        ..Default::default()
    };
    SiriGuard::new(cfg, Some(Box::new(det))).unwrap()
}

const HEY_SIRI: [&str; 4] = ["hey_siri_1", "hey_siri_2", "hey_siri_3", "hey_siri_4"];

#[test]
fn hey_siri_is_notched_from_the_s_on() {
    for name in HEY_SIRI {
        let x = clip(name);
        let mut det = HeySiriDetector::default();
        let regs = regions(&mut det, &x, 480);
        let st = det.stats();
        assert_eq!(st.confirmed, 1, "{name}: {st:?}");
        // one continuous region of the full length, from 30 ms before the /s/
        let start = regs.iter().map(|r| r.0).min().unwrap();
        let end = regs.iter().map(|r| r.1).max().unwrap();
        let secs = covered(&regs);
        assert!((secs - 0.48).abs() < 1e-6, "{name}: {secs}");
        assert_eq!(end - start, (0.48 * FS as f64) as i64, "{name}");

        let mut g = guard(HeySiriDetector::default());
        let y = g.process_signal(&x, 480);
        assert_eq!(g.detections(), 1, "{name}");
        assert_eq!(g.clamped(), 0, "{name}");
        // the /s/ (first 60 ms after the onset) lost its 4.2-6.3 kHz energy ...
        let s = (start + det.pre()) as usize;
        let n = FS * 6 / 100;
        let drop = band_energy_db(&x[s..s + n], 4200.0, 6300.0)
            - band_energy_db(&y[s..s + n], 4200.0, 6300.0);
        println!("[metrics] heysiri[{name}]: s_band_drop_dB={drop:.1}");
        assert!(drop > 20.0, "{name}: {drop}");
        // ... and the "hey" before it is untouched
        let before = &x[..s - FS / 10];
        let diff = before
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(diff < 1e-4, "{name}: {diff}");
    }
}

/// The "early" profile: the region also covers the 0.2 s before the /s/.
#[test]
fn reaching_back_also_notches_the_hey() {
    for name in HEY_SIRI {
        let x = clip(name);
        let mut plain = HeySiriDetector::default();
        let from_s = regions(&mut plain, &x, 480);
        let mut det = HeySiriDetector::reaching_back();
        let regs = regions(&mut det, &x, 480);
        assert_eq!(det.stats(), plain.stats(), "{name}");
        let onset = from_s.iter().map(|r| r.0).min().unwrap() + plain.pre();
        let start = regs.iter().map(|r| r.0).min().unwrap();
        assert_eq!(onset - start, (0.2 * FS as f64) as i64, "{name}");
        assert!((covered(&regs) - 0.65).abs() < 1e-6, "{name}");

        let mut g = guard(HeySiriDetector::reaching_back());
        let y = g.process_signal(&x, 480);
        assert_eq!((g.detections(), g.clamped()), (1, 0), "{name}");
        // the 0.15 s before the /s/ (the vowel of "hey") lost its 1.2-3 kHz
        let (a, b) = (onset as usize - FS * 15 / 100, onset as usize);
        let drop =
            band_energy_db(&x[a..b], 1200.0, 3000.0) - band_energy_db(&y[a..b], 1200.0, 3000.0);
        println!("[metrics] heysiri_early[{name}]: hey_band_drop_dB={drop:.1}");
        assert!(drop > 20.0, "{name}: {drop}");
    }
}

#[test]
fn mandarin_speech_is_left_alone() {
    let x = clip("mandarin");
    let secs = x.len() as f64 / FS as f64;
    let mut det = HeySiriDetector::default();
    let hey = covered(&regions(&mut det, &x, 480));
    let st = det.stats();
    println!("[metrics] heysiri[mandarin]: seconds={secs:.1}, notched_s={hey:.2}, {st:?}");
    // plenty of /s/-like onsets, none of them taken for "hey siri"
    assert!(st.onsets >= 10, "{st:?}");
    assert_eq!(st.confirmed, 0, "{st:?}");
    assert!(hey < 0.02 * secs, "{hey}");
}

#[test]
fn regions_do_not_depend_on_the_block_size() {
    let mut x = clip(HEY_SIRI[0]);
    x.extend(clip("mandarin"));
    let want = regions(&mut HeySiriDetector::default(), &x, 480);
    assert!(!want.is_empty());
    for block in [1usize, 7, 333, 479, 481, 1000, 4096] {
        let got = regions(&mut HeySiriDetector::default(), &x, block);
        assert_eq!(got, want, "block {block}");
        let mut g = guard(HeySiriDetector::default());
        let y = g.process_signal(&x, block);
        assert!(y.iter().all(|v| v.is_finite()));
        assert_eq!(g.clamped(), 0, "block {block}");
    }
}

#[test]
fn latency() {
    assert_eq!(guard(HeySiriDetector::default()).latency(), 3479); // 72.5 ms
    assert_eq!(guard(HeySiriDetector::reaching_back()).latency(), 11639); // 242.5 ms
}
