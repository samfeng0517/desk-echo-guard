//! Thread-safe elastic buffer between devices running on different clocks
//! (`buffers.ElasticBuffer`).
//!
//! Every physical / virtual device has its own crystal, so two nominally
//! 48 kHz streams differ by tens to hundreds of ppm.  The producer pushes
//! whatever it gets, the consumer pops exactly `n` samples, and the read side
//! resamples by `1 + u` (|u| <= max_ppm, Catmull-Rom cubic interpolation).
//! `u` comes from a critically damped PI controller on the fill level, so the
//! level settles at `target` and the integral term converges to the true
//! clock ratio.
//!
//! Devices deliver fixed-size packets, so the raw level is a staircase.  The
//! controller uses a *continuous* level estimate instead: the producer's
//! progress since its last push is interpolated from wall-clock time, which
//! removes the packet-size sawtooth.  Large jumps (a producer that stalls and
//! then resumes) are handled by hard resync / re-priming instead of slow
//! steering.

use parking_lot::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::FS;

/// Time source in seconds (injectable for deterministic tests).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Front-trimming threshold: keeps the backing buffer short without moving
/// memory on every pop.
const TRIM: usize = 4096;

fn monotonic_seconds() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[derive(Clone)]
pub struct ElasticOptions {
    /// Level above `target + hard_excess` drops samples back to target at
    /// once (default: `max(4 * target, 2400)`).
    pub hard_excess: Option<usize>,
    /// Controller time constant (critically damped PI loop).
    pub tau_s: f64,
    /// Time constant of the level-estimate low-pass (timing jitter).
    pub smooth_s: f64,
    /// Time source; `None` = monotonic real-time clock.
    pub clock: Option<Clock>,
}

impl Default for ElasticOptions {
    fn default() -> Self {
        Self {
            hard_excess: None,
            tau_s: 6.0,
            smooth_s: 0.5,
            clock: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ElasticStats {
    pub level: f64,
    pub ppm: f64,
    pub drift_ppm: f64,
    pub underruns: u64,
    pub resyncs: u64,
}

struct State {
    /// `data[0..]` with one sample of history in front of the read position
    /// for the cubic kernel.
    data: Vec<f32>,
    pos: f64,
    primed: bool,
    level_f: f64,
    integ: f64,
    t_push: Option<f64>,
    last_chunk: usize,
    ratio: f64,
    underruns: u64,
    resyncs: u64,
    pushed: u64,
    popped: u64,
}

pub struct ElasticBuffer {
    target: usize,
    max_dev: f64,
    hard_excess: usize,
    fs: f64,
    kp: f64,
    ki: f64,
    smooth_s: f64,
    clock: Clock,
    st: Mutex<State>,
}

impl ElasticBuffer {
    /// `target`: desired fill level in samples (also the added latency);
    /// `max_ppm`: max resampling correction.  Real-time clock.
    pub fn new(target: usize, max_ppm: f64) -> Self {
        Self::with_options(target, max_ppm, ElasticOptions::default())
    }

    pub fn with_options(target: usize, max_ppm: f64, opts: ElasticOptions) -> Self {
        let fs = FS as f64;
        let tau = opts.tau_s;
        Self {
            target,
            max_dev: max_ppm * 1e-6,
            hard_excess: opts.hard_excess.unwrap_or((4 * target).max(2400)),
            fs,
            // e'' = -fs*kp*e' - fs*ki*e  ->  w = 1/tau, zeta = 1
            kp: 2.0 / (tau * fs),
            ki: 1.0 / (tau * tau * fs),
            smooth_s: opts.smooth_s,
            clock: opts.clock.unwrap_or_else(|| Arc::new(monotonic_seconds)),
            st: Mutex::new(State {
                data: {
                    let mut v = Vec::with_capacity(2 * TRIM + 4 * target);
                    v.push(0.0);
                    v
                },
                pos: 1.0,
                primed: false,
                level_f: 0.0,
                integ: 0.0,
                t_push: None,
                last_chunk: 0,
                ratio: 1.0,
                underruns: 0,
                resyncs: 0,
                pushed: 0,
                popped: 0,
            }),
        }
    }

    pub fn target(&self) -> usize {
        self.target
    }

    // -- producer -----------------------------------------------------------
    pub fn push(&self, x: &[f32]) {
        let now = (self.clock)();
        self.push_at(x, now);
    }

    pub fn push_at(&self, x: &[f32], now: f64) {
        let mut s = self.st.lock();
        s.data.extend_from_slice(x);
        s.pushed += x.len() as u64;
        s.t_push = Some(now);
        s.last_chunk = x.len();
    }

    // -- consumer -----------------------------------------------------------
    /// Fill `out` completely (silence until primed / during underruns).
    pub fn pop_into(&self, out: &mut [f32]) {
        let now = (self.clock)();
        self.pop_at(out, now);
    }

    pub fn pop_at(&self, out: &mut [f32], now: f64) {
        let mut s = self.st.lock();
        self.pop_locked(&mut s, out, now);
    }

    /// Raw fill level in samples (fractional read position).
    pub fn level(&self) -> f64 {
        let s = self.st.lock();
        s.data.len() as f64 - s.pos
    }

    /// Current resampling ratio `1 + u`.
    pub fn ratio(&self) -> f64 {
        self.st.lock().ratio
    }

    pub fn underruns(&self) -> u64 {
        self.st.lock().underruns
    }

    pub fn resyncs(&self) -> u64 {
        self.st.lock().resyncs
    }

    /// Total samples pushed / popped (diagnostics).
    pub fn counters(&self) -> (u64, u64) {
        let s = self.st.lock();
        (s.pushed, s.popped)
    }

    pub fn stats(&self) -> ElasticStats {
        let s = self.st.lock();
        ElasticStats {
            level: s.data.len() as f64 - s.pos,
            ppm: (s.ratio - 1.0) * 1e6,
            drift_ppm: s.integ * 1e6,
            underruns: s.underruns,
            resyncs: s.resyncs,
        }
    }

    fn continuous_level(&self, s: &State, now: f64) -> f64 {
        let level = s.data.len() as f64 - s.pos;
        match s.t_push {
            None => level,
            Some(tp) => {
                let since = (now - tp).max(0.0) * self.fs;
                let chunk = s.last_chunk as f64;
                level - chunk + since.min(chunk)
            }
        }
    }

    /// Jump the read position so the continuous level equals target.
    fn reset_to_target(&self, s: &mut State, now: f64) {
        s.pos += self.continuous_level(s, now) - self.target as f64;
        s.level_f = self.target as f64;
    }

    fn pop_locked(&self, s: &mut State, out: &mut [f32], now: f64) {
        let n = out.len();
        if n == 0 {
            return;
        }
        let target = self.target as f64;
        let level = s.data.len() as f64 - s.pos;
        if !s.primed {
            if self.continuous_level(s, now) < target {
                out.fill(0.0);
                return;
            }
            // start exactly at target for a reproducible latency
            self.reset_to_target(s, now);
            s.primed = true;
        } else if level > target + self.hard_excess as f64 {
            self.reset_to_target(s, now);
            s.resyncs += 1;
        }

        let dt = n as f64 / self.fs;
        let a = if self.smooth_s > 0.0 {
            (dt / self.smooth_s).min(1.0)
        } else {
            1.0
        };
        let cl = self.continuous_level(s, now);
        s.level_f += a * (cl - s.level_f);
        let e = s.level_f - target;
        // anti-windup: an underrunning producer says nothing about its clock
        if s.data.len() as f64 - s.pos >= n as f64 {
            s.integ = (s.integ + self.ki * e * dt).clamp(-self.max_dev, self.max_dev);
        }
        let u = (self.kp * e + s.integ).clamp(-self.max_dev, self.max_dev);
        s.ratio = 1.0 + u;

        // Underrun: the consumer's clock is the master, so time keeps
        // running - missing samples are played as silence and the read
        // position still advances.  Late samples that arrive for that span
        // are skipped, which keeps ref/mic alignment unchanged.
        let d = &s.data;
        let len = d.len() as i64;
        let last = (s.pos + s.ratio * (n - 1) as f64).floor() as i64;
        if last + 3 - len > 0 {
            s.underruns += 1;
        }
        let at = |j: i64| -> f32 {
            if j >= 0 && j < len {
                d[j as usize]
            } else {
                0.0
            }
        };
        for (k, o) in out.iter_mut().enumerate() {
            let p = s.pos + s.ratio * k as f64;
            let i = p.floor();
            let t = (p - i) as f32;
            let i = i as i64;
            let (y0, y1, y2, y3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            // Catmull-Rom spline
            let c3 = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
            let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
            let c1 = -0.5 * y0 + 0.5 * y2;
            *o = ((c3 * t + c2) * t + c1) * t + y1;
        }

        s.pos += s.ratio * n as f64;
        let drop = (s.pos as i64 - 1).min(s.data.len() as i64 - 1);
        if drop > TRIM as i64 {
            s.data.drain(..drop as usize);
            s.pos -= drop as f64;
        }
        if s.data.len() as f64 - s.pos < -(self.hard_excess as f64) {
            // producer stopped for a long time: start over (re-prime)
            let keep = *s.data.last().unwrap_or(&0.0);
            s.data.clear();
            s.data.push(keep);
            s.pos = 1.0;
            s.primed = false;
            s.t_push = None;
            s.integ = 0.0;
            s.resyncs += 1;
        }
        s.popped += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::best_lag;
    use rand::{rngs::StdRng, SeedableRng};
    use rand_distr::{Distribution, StandardNormal};

    fn fixed_clock() -> ElasticOptions {
        ElasticOptions {
            clock: Some(Arc::new(|| 0.0)),
            ..Default::default()
        }
    }

    #[test]
    fn passthrough_is_transparent_without_drift() {
        let b = ElasticBuffer::with_options(960, 1000.0, fixed_clock());
        let mut rng = StdRng::seed_from_u64(0);
        let x: Vec<f32> = (0..48000 * 3)
            .map(|_| 0.2 * Distribution::<f64>::sample(&StandardNormal, &mut rng) as f32)
            .collect();
        let mut y = vec![0.0f32; x.len()];
        for (k, i) in (0..x.len()).step_by(480).enumerate() {
            b.push_at(&x[i..i + 480], k as f64 * 0.01);
            b.pop_at(&mut y[i..i + 480], k as f64 * 0.01 + 0.004);
        }
        let lag = best_lag(&x[..96000], &y[..96000], 4000);
        let lag = usize::try_from(lag).expect("output must lag the input");
        let err = (48000..96000)
            .map(|j| (y[j] - x[j - lag]).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 1e-3, "max err {err}");
        assert_eq!(b.underruns(), 0);
        assert!((b.ratio() - 1.0).abs() < 1e-9);
    }

    /// Deterministic producer (clock 1+ppm, 480-sample chunks) vs consumer.
    fn drift_run(ppm: f64, seconds: f64, target: usize) -> (ElasticBuffer, Vec<f64>, Vec<f32>) {
        let b = ElasticBuffer::with_options(target, 1000.0, fixed_clock());
        let fs = 48000.0;
        let r = 1.0 + ppm * 1e-6;
        let n = (seconds * fs * r) as usize + 4800;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                (0.3 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / (fs * r)).sin()) as f32
            })
            .collect();
        let (mut t_prod, dtp, mut pp) = (0.0, 480.0 / (fs * r), 0usize);
        let steps = (seconds * 100.0) as usize;
        let mut levels = Vec::with_capacity(steps);
        let mut outs = vec![0.0f32; steps * 480];
        for k in 0..steps {
            let t = (k + 1) as f64 * 0.01;
            while t_prod <= t {
                b.push_at(&x[pp..(pp + 480).min(n)], t_prod);
                pp += 480;
                t_prod += dtp;
            }
            b.pop_at(&mut outs[k * 480..(k + 1) * 480], t);
            levels.push(b.level());
        }
        (b, levels, outs)
    }

    #[test]
    fn tracks_clock_drift() {
        for ppm in [500.0, -500.0] {
            let (b, levels, y) = drift_run(ppm, 60.0, 960);
            let st = b.stats();
            let tail = &levels[levels.len() - 1000..];
            let lmin = tail.iter().copied().fold(f64::INFINITY, f64::min);
            let lmax = tail.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            println!(
                "[metrics] elastic_drift: true_ppm={ppm:.2}, estimated_ppm={:.2}, \
                 level_min_ms={:.2}, level_max_ms={:.2}, underruns={}",
                st.drift_ppm,
                lmin / 48.0,
                lmax / 48.0,
                st.underruns
            );
            assert!((st.drift_ppm - ppm).abs() < 30.0);
            assert!(st.underruns <= 1 && st.resyncs == 0);
            assert!(lmin > 0.0 && lmax < 3.0 * 960.0);
            // output is still a clean 440 Hz tone (spectral purity)
            let seg = &y[y.len() - 48000..];
            let m = seg.len();
            let w: Vec<f64> = seg
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    // np.hanning (symmetric)
                    let h =
                        0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (m - 1) as f64).cos();
                    v as f64 * h
                })
                .collect();
            let spec = crate::filters::rfft_power(&w);
            let k = (440.0 * m as f64 / 48000.0).round() as usize;
            let tone: f64 = spec[k - 3..k + 4].iter().sum();
            let total: f64 = spec.iter().sum();
            assert!(10.0 * (tone / (total - tone)).log10() > 40.0);
        }
    }

    #[test]
    fn underrun_keeps_time_alignment() {
        let b = ElasticBuffer::with_options(400, 1000.0, fixed_clock());
        let ramp: Vec<f32> = (0..3000).map(|i| i as f32).collect();
        b.push_at(&ramp[..1000], 0.0);
        let mut first = vec![0.0; 300];
        b.pop_at(&mut first, 1.0); // continuous level = 1000 -> primed at 400
        assert_eq!(first[0], 600.0);
        assert_eq!(b.underruns(), 0);
        b.pop_at(&mut first, 1.01); // only 100 samples left: underrun
        assert_eq!(b.underruns(), 1);
        b.push_at(&ramp[1000..], 1.02);
        let mut nxt = vec![0.0; 300];
        b.pop_at(&mut nxt, 1.03);
        // the read position kept advancing through the underrun
        assert!((nxt[0] - (600.0 + 600.0)).abs() < 2.0, "{}", nxt[0]);
    }

    #[test]
    fn reprimes_after_long_stall_and_is_thread_safe() {
        let b = Arc::new(ElasticBuffer::with_options(480, 1000.0, fixed_clock()));
        b.push_at(&[0.5; 960], 0.0);
        let mut out = vec![0.0; 480];
        b.pop_at(&mut out, 0.01);
        // nothing pushed for a long time: level falls below -hard_excess
        for k in 0..20 {
            b.pop_at(&mut out, 0.02 + k as f64 * 0.01);
        }
        assert!(b.resyncs() >= 1);
        b.pop_at(&mut out, 1.0);
        assert!(out.iter().all(|&v| v == 0.0), "not primed: silence");
        let p = Arc::clone(&b);
        std::thread::spawn(move || p.push(&[0.25; 4800]))
            .join()
            .unwrap();
        assert!(b.level() > 4000.0);
    }
}
