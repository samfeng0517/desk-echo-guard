//! `--siri-test DIR`: files for a listening test on a real HomePod / iPhone.
//!
//! Nothing but a real device tells whether a setting keeps Siri asleep, so
//! this writes a folder of numbered wav files: the same "Hey Siri" clips,
//! unprocessed (the control) and processed by each mode / strength, in a
//! shuffled order, each followed by a pause.  The user plays them through
//! the speakers, notes which numbers woke Siri and looks them up in the
//! answer file.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use echoguard_core::{make_guard, Settings};
use echoguard_dsp::{IntegerResampler, FRAME, FS};

use crate::wav::{read_wav, write_wav};

/// Built-in synthetic "hey siri" clips (16 kHz s16le; CC-BY-4.0, see
/// `crates/dsp/tests/fixtures/heysiri/README.md`).
const BUILT_IN: [(&str, &[u8]); 4] = [
    (
        "內建女聲 A",
        include_bytes!("../../dsp/tests/fixtures/heysiri/hey_siri_1.i16"),
    ),
    (
        "內建男聲 B",
        include_bytes!("../../dsp/tests/fixtures/heysiri/hey_siri_2.i16"),
    ),
    (
        "內建聲音 C",
        include_bytes!("../../dsp/tests/fixtures/heysiri/hey_siri_3.i16"),
    ),
    (
        "內建聲音 D",
        include_bytes!("../../dsp/tests/fixtures/heysiri/hey_siri_4.i16"),
    ),
];

/// (label, siri_mode, siri_profile); `None` = unprocessed.
const VARIANTS: [(&str, Option<(&str, &str)>); 4] = [
    ("原音（對照組，應該要叫醒 Siri）", None),
    ("一直處理 + 標準", Some(("always", "deep-wide"))),
    (
        "只在 Hey Siri 時處理 + 標準",
        Some(("heysiri", "deep-wide")),
    ),
    ("只在 Hey Siri 時處理 + 加強", Some(("heysiri", "early"))),
];

/// Silence before the phrase (also the detector's run-in) and after it (the
/// pause before the next file; a near miss makes Siri more sensitive for a
/// few seconds).
const LEAD_S: f64 = 1.0;
const PAUSE_S: f64 = 12.0;
/// Peak level of every clip before processing.
const PEAK: f32 = 0.5;

struct Trial {
    variant: usize,
    voice: usize,
    /// regions the detector processed (`None` for the control / always)
    detected: Option<u64>,
    audio: Vec<f32>,
}

fn normalized(mut x: Vec<f32>) -> Vec<f32> {
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak > 0.0 {
        x.iter_mut().for_each(|v| *v *= PEAK / peak);
    }
    x
}

fn built_in(raw: &[u8]) -> Vec<f32> {
    let x: Vec<f32> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
        .collect();
    let mut rs = IntegerResampler::new(-3);
    let mut y = rs.process(&x);
    y.extend(rs.process(&[0.0; 1600])); // flush the filter
    y
}

/// One file: silence, the (processed) phrase, the pause.
fn render(clip: &[f32], variant: Option<(&str, &str)>) -> Result<(Vec<f32>, Option<u64>)> {
    let lead = (LEAD_S * FS as f64) as usize;
    let mut x = vec![0.0f32; lead];
    x.extend_from_slice(clip);
    x.extend(std::iter::repeat_n(0.0, FS)); // room for the end of the notch region
    let (mut y, detected) = match variant {
        None => (x, None),
        Some((mode, profile)) => {
            let mut g = make_guard(&Settings {
                siri_mode: mode.into(),
                siri_profile: profile.into(),
                ..Default::default()
            })?;
            let y = g.process_signal(&x, FRAME);
            (y, (mode == "heysiri").then_some(g.detections()))
        }
    };
    y.extend(std::iter::repeat_n(
        0.0,
        (PAUSE_S * FS as f64) as usize - FS,
    ));
    Ok((y, detected))
}

/// Deterministic shuffle (xorshift), so the same input gives the same order.
fn shuffle<T>(items: &mut [T]) {
    let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ items.len() as u64;
    for i in (1..items.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        items.swap(i, (s % (i as u64 + 1)) as usize);
    }
}

/// Write the test files into `dir`; returns the number of wav files.
pub fn run(dir: &Path, clips: &[PathBuf], repeats: usize) -> Result<usize> {
    let repeats = repeats.clamp(1, 10);
    let mut voices: Vec<(String, Vec<f32>)> = Vec::new();
    for p in clips {
        let x = read_wav(p)?;
        if x.len() < FS / 4 || x.len() > 6 * FS {
            bail!(
                "{}: a clip should hold one \"Hey Siri\" (0.25 to 6 s), this one is {:.1} s",
                p.display(),
                x.len() as f64 / FS as f64
            );
        }
        let name = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        voices.push((format!("你的錄音 {name}"), normalized(x)));
    }
    for (name, raw) in BUILT_IN {
        voices.push((name.to_string(), normalized(built_in(raw))));
    }
    let mut trials = Vec::new();
    for (vi, (_, clip)) in voices.iter().enumerate() {
        for (ki, (_, variant)) in VARIANTS.iter().enumerate() {
            let (audio, detected) = render(clip, *variant)?;
            for _ in 0..repeats {
                trials.push(Trial {
                    variant: ki,
                    voice: vi,
                    detected,
                    audio: audio.clone(),
                });
            }
        }
    }
    shuffle(&mut trials);

    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for (i, t) in trials.iter().enumerate() {
        write_wav(&dir.join(format!("{:02}.wav", i + 1)), &t.audio)?;
    }
    let n = trials.len();
    let minutes = (n as f64 * (LEAD_S + PAUSE_S + 1.0) / 60.0).ceil();

    let mut sheet = String::from("\u{feff}");
    let _ = write!(
        sheet,
        "Hey Siri 實機測試：說明與記錄表\r\n\
         ================================\r\n\r\n\
         這個資料夾有 {n} 個音檔（01.wav 到 {n:02}.wav）。每個檔案是一句「Hey Siri」，\r\n\
         有的沒處理過、有的用不同的設定處理過，順序是打亂的。播完全部約 {minutes:.0} 分鐘。\r\n\r\n\
         怎麼測\r\n\
         ------\r\n\
         1. 把 HomePod / iPhone 放在平常的位置，確認「嘿 Siri」是開的。\r\n\
         2. 用任何播放器（例如 Windows 的「媒體播放器」）從你平常聽通話的喇叭播放，\r\n\
         \x20  音量調到平常通話的大小。不要選「Siri防護喇叭」，要直接從真正的喇叭播。\r\n\
         3. 照編號一個一個播（或整個資料夾連續播）。每個檔案後面有 12 秒的空白，\r\n\
         \x20  Siri 被叫醒的話，在這段時間把它關掉。\r\n\
         4. 在下面的表格記下每一個編號有沒有叫醒 Siri。先不要看「答案.txt」。\r\n\
         5. 全部播完再打開「答案.txt」，照裡面的分組數一數每種設定被叫醒幾次。\r\n\r\n\
         注意\r\n\
         ----\r\n\
         * iPhone 會比對是不是主人的聲音，內建的合成聲音很可能叫不醒它。用 HomePod 測，\r\n\
         \x20 或是自己錄一句「Hey Siri」（wav 檔）再重新產生：\r\n\
         \x20   DeskEchoGuard.exe --siri-test 資料夾 --siri-test-clip 我的錄音.wav\r\n\
         * 「原音」那幾題如果都沒叫醒 Siri，這次測試就不能用（Siri 本來就不理這個聲音）。\r\n\
         * 想測得更準：每種組合多播幾次（--siri-test-repeats 4），或多錄幾個人的聲音。\r\n\r\n\
         記錄表（有叫醒就在 [ ] 裡打勾）\r\n\
         ------------------------------\r\n"
    );
    for i in 1..=n {
        let _ = write!(sheet, "{i:02}  [ ]\r\n");
    }
    std::fs::write(dir.join("說明與記錄表.txt"), sheet)?;

    let mut key = String::from("\u{feff}");
    let _ = write!(
        key,
        "Hey Siri 實機測試：答案（測完再看）\r\n\
         ====================================\r\n\r\n\
         每種設定的題號\r\n\
         --------------\r\n"
    );
    for (ki, (label, _)) in VARIANTS.iter().enumerate() {
        let nums: Vec<String> = trials
            .iter()
            .enumerate()
            .filter(|(_, t)| t.variant == ki)
            .map(|(i, _)| format!("{:02}", i + 1))
            .collect();
        let _ = write!(
            key,
            "{label}\r\n    題號：{}\r\n    被叫醒：____ / {} 次\r\n\r\n",
            nums.join(", "),
            nums.len()
        );
    }
    let _ = write!(
        key,
        "怎麼看結果\r\n\
         ----------\r\n\
         * 「原音」應該大部分都會叫醒 Siri；不是的話，這次測試不能用。\r\n\
         * 某個設定被叫醒的次數越少越好。只播幾次的話，差 1、2 次不代表什麼。\r\n\
         * 標了「偵測器沒抓到」的題目等於沒處理過，那一題被叫醒是偵測器漏掉，不是削法沒用。\r\n\r\n\
         每一題\r\n\
         ------\r\n"
    );
    for (i, t) in trials.iter().enumerate() {
        let note = match t.detected {
            Some(0) => "（偵測器沒抓到這一句）",
            _ => "",
        };
        let _ = write!(
            key,
            "{:02}  {}｜{}{note}\r\n",
            i + 1,
            VARIANTS[t.variant].0,
            voices[t.voice].0
        );
    }
    std::fs::write(dir.join("答案.txt"), key)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_shuffled_trials_sheet_and_key() {
        let dir = std::env::temp_dir().join(format!("deg-siritest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // an own recording: one of the built-in clips saved as a wav
        let own = dir.join("own.wav");
        std::fs::create_dir_all(&dir).unwrap();
        write_wav(&own, &built_in(BUILT_IN[0].1)).unwrap();
        let out = dir.join("test");
        let n = run(&out, std::slice::from_ref(&own), 1).unwrap();
        assert_eq!(n, 5 * VARIANTS.len());
        let len = ((LEAD_S + PAUSE_S) * FS as f64) as usize;
        for i in 1..=n {
            let x = read_wav(&out.join(format!("{i:02}.wav"))).unwrap();
            assert!(x.len() > len && x.len() < len + 3 * FS, "{i}: {}", x.len());
            // the pause really is silent
            assert!(x[x.len() - 10 * FS..].iter().all(|v| v.abs() < 1e-4), "{i}");
        }
        let key = std::fs::read_to_string(out.join("答案.txt")).unwrap();
        for (label, _) in VARIANTS {
            assert!(key.contains(label), "{label}");
        }
        assert!(key.contains("你的錄音 own.wav"));
        // the detector found the phrase in every built-in clip
        assert!(!key.contains("偵測器沒抓到這一句"), "{key}");
        // not in generation order
        let order: Vec<&str> = key
            .lines()
            .filter(|l| l.len() > 4 && l.as_bytes()[2] == b' ' && l.contains('｜'))
            .collect();
        assert_eq!(order.len(), n);
        assert!(!order.chunks(VARIANTS.len()).all(|c| c
            .iter()
            .all(|l| l.contains(c[0].split('｜').nth(1).unwrap()))));
        let sheet = std::fs::read_to_string(out.join("說明與記錄表.txt")).unwrap();
        assert!(sheet.contains(&format!("{n:02}  [ ]")));
        // a clip that is far too long is refused
        let long = dir.join("long.wav");
        write_wav(&long, &vec![0.1; 7 * FS]).unwrap();
        assert!(run(&out, &[long], 1).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
