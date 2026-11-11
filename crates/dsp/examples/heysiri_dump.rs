//! Training helper for the "Hey Siri" detector (see `tools/heysiri`): runs
//! the detector front end over raw 48 kHz mono f32le files and dumps the
//! log-mel window around every /s/ onset.
//!
//! `heysiri_dump <list.txt> <out.bin>`; per input file the output holds
//! `u32 n` and n × (`i32 onset_frame`, `(PRE + 1 + POST) * NMEL` f32).

use echoguard_dsp::heysiri::{MelFrontend, SibilantFrame, HOP, NMEL, PRE_FRAMES, STAGE_POST};
use std::io::Write;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let list = std::fs::read_to_string(&args[1])?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(&args[2])?);
    let post = STAGE_POST[2];
    for path in list.lines().filter(|l| !l.is_empty()) {
        let raw = std::fs::read(path)?;
        let x: Vec<f32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let mut sib = SibilantFrame::sensitive();
        let mut mel = MelFrontend::new();
        let silence = (1e-10f32).ln();
        let mut feats: Vec<[f32; NMEL]> = vec![[silence; NMEL]; PRE_FRAMES];
        let mut onsets = Vec::new();
        let mut prev = false;
        for (k, f) in x.chunks_exact(HOP).enumerate() {
            let s = sib.is_sibilant(f);
            if s && !prev {
                onsets.push(k);
            }
            prev = s;
            let mut m = [0.0; NMEL];
            mel.frame(f, &mut m);
            feats.push(m);
        }
        feats.extend(std::iter::repeat_n([silence; NMEL], post));
        out.write_all(&(onsets.len() as u32).to_le_bytes())?;
        for k in onsets {
            out.write_all(&(k as i32).to_le_bytes())?;
            // frame k sits at feats[k + PRE_FRAMES]
            for m in &feats[k..=k + PRE_FRAMES + post] {
                for v in m {
                    out.write_all(&v.to_le_bytes())?;
                }
            }
        }
    }
    Ok(())
}
