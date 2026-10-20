//! Platform-independent parts of the device streams: the blocking side of
//! the lock-free SPSC rings that connect a device thread to the pipeline,
//! sample-format / channel conversion, ring sizing and the idle-loopback
//! silence bookkeeping.  Kept free of OS calls so it is unit-tested on every
//! platform; `wasapi.rs` only wires it to the device.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::{Condvar, Mutex};
use rtrb::{Consumer, Producer};

/// Nominal stream rate (`dsp::FS`; this crate does not depend on dsp).
pub const FS: usize = 48_000;

/// Upper bound for one condvar wait.  Only a safety net: every state change
/// (data, space, error, stop) notifies, so waits normally end earlier.
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// State shared between a device thread and the stream handle.
#[derive(Default)]
pub struct Shared {
    /// Set by the handle on drop; the device thread exits.
    stop: AtomicBool,
    /// Set by the device thread when it exits (error or stop); after that
    /// nothing will move through the ring any more.
    dead: AtomicBool,
    error: Mutex<Option<String>>,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Shared {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wake whoever blocks on the ring.  Call after every push / pop on the
    /// device side.
    pub fn notify(&self) {
        // Taking the lock orders this notify after a waiter's re-check (see
        // `wait_while`), so no wake-up is lost.
        let _g = self.lock.lock();
        self.cv.notify_all();
    }

    /// Block while `cond()` holds and the stream is alive (bounded by
    /// `WAIT_SLICE`; callers loop).
    pub fn wait_while(&self, cond: impl Fn() -> bool) {
        let mut g = self.lock.lock();
        if cond() && !self.is_dead() {
            self.cv.wait_for(&mut g, WAIT_SLICE);
        }
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.notify();
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Device thread: record a fatal error (first one wins) and die.
    pub fn fail(&self, msg: String) {
        {
            let mut e = self.error.lock();
            if e.is_none() {
                *e = Some(msg);
            }
        }
        self.mark_dead();
    }

    /// Device thread: mark the stream finished (always called on exit).
    pub fn mark_dead(&self) {
        self.dead.store(true, Ordering::SeqCst);
        self.notify();
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Error to hand to the pipeline once the stream is dead.
    pub fn dead_error(&self, what: &str) -> anyhow::Error {
        match self.error.lock().clone() {
            Some(e) => anyhow!("{what}: {e}"),
            None => anyhow!("{what}: stream stopped"),
        }
    }
}

/// Consumer side of a capture ring: `read` blocks until `out` is full.
pub fn read_blocking(
    cons: &mut Consumer<f32>,
    shared: &Shared,
    what: &str,
    out: &mut [f32],
) -> Result<()> {
    let mut filled = 0;
    while filled < out.len() {
        let (popped, _) = cons.pop_partial_slice(&mut out[filled..]);
        filled += popped.len();
        if filled == out.len() {
            break;
        }
        if shared.is_dead() && cons.is_empty() {
            return Err(shared.dead_error(what));
        }
        shared.wait_while(|| cons.is_empty());
    }
    Ok(())
}

/// Producer side of a render ring: `write` blocks until all of `x` is queued.
pub fn write_blocking(
    prod: &mut Producer<f32>,
    shared: &Shared,
    what: &str,
    x: &[f32],
) -> Result<()> {
    let mut rest = x;
    loop {
        // Fail fast: once the device thread is gone nothing drains the ring.
        if shared.is_dead() {
            return Err(shared.dead_error(what));
        }
        let (_, r) = prod.push_partial_slice(rest);
        rest = r;
        if rest.is_empty() {
            return Ok(());
        }
        shared.wait_while(|| prod.is_full());
    }
}

/// Device side of a capture ring: push what fits, return how many samples
/// were dropped because the consumer fell behind.
pub fn push_or_drop(prod: &mut Producer<f32>, x: &[f32]) -> usize {
    let (_, rest) = prod.push_partial_slice(x);
    rest.len()
}

/// Capture ring capacity.  It does not add latency (the reader drains it as
/// soon as a block is complete) but bounds how much a stalled reader can
/// accumulate before samples are dropped: a few blocks plus two device
/// buffers (a device may hand over a whole buffer at once).
pub fn capture_ring_capacity(blocksize: usize, device_buffer_frames: usize) -> usize {
    4 * blocksize.max(1) + 2 * device_buffer_frames
}

/// Render ring capacity.  `write` blocks once it is full, so this *is* the
/// latency added on top of the device buffer: one block (the unit the
/// pipeline writes) plus one device period of headroom, so the writer can
/// queue the next block while the device thread is waiting for its event.
pub fn render_ring_capacity(blocksize: usize, period_frames: usize) -> usize {
    blocksize.max(period_frames).max(1) + period_frames
}

/// Device buffer to request from WASAPI (shared mode treats it as a
/// minimum).  Render: two periods, or one block if larger; the render
/// thread also never fills beyond this (see `render_fill_target`), so it
/// bounds the device-side latency.  Capture: generous (100 ms), it only
/// absorbs scheduling hiccups of the capture thread and adds no latency.
pub fn device_buffer_frames(render: bool, blocksize: usize, period_frames: usize) -> usize {
    if render {
        render_fill_target(blocksize, period_frames)
    } else {
        (FS / 10).max(4 * period_frames)
    }
}

/// How many frames the render thread keeps queued in the device buffer.
pub fn render_fill_target(blocksize: usize, period_frames: usize) -> usize {
    (2 * period_frames).max(blocksize).max(1)
}

/// Frames to hand to the render device now: limited by the free device
/// space, by the fill target and by what the pipeline has queued.
pub fn render_frames_now(target: usize, buffer: usize, padding: usize, queued: usize) -> usize {
    let free = buffer.saturating_sub(padding);
    target.saturating_sub(padding).min(free).min(queued)
}

/// Frames -> 100 ns units at `FS`.
pub fn frames_to_hns(frames: usize) -> i64 {
    (frames as i64 * 10_000_000) / FS as i64
}

/// 100 ns units -> frames at `FS` (rounded up).
pub fn hns_to_frames(hns: i64) -> usize {
    let hns = hns.max(0) as u64;
    (hns * FS as u64).div_ceil(10_000_000) as usize
}

/// Interleaved little-endian f32 frames -> mono (channel mean), appended to
/// `out`.  `silent` (AUDCLNT_BUFFERFLAGS_SILENT) yields zeros.
pub fn bytes_to_mono(bytes: &[u8], channels: usize, silent: bool, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    let frame_bytes = 4 * channels;
    let frames = bytes.len() / frame_bytes;
    if silent {
        out.resize(out.len() + frames, 0.0);
        return;
    }
    let scale = 1.0 / channels as f32;
    out.extend(bytes.chunks_exact(frame_bytes).map(|frame| {
        let sum: f32 = frame
            .chunks_exact(4)
            .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .sum();
        sum * scale
    }));
}

/// Mono -> interleaved little-endian f32 frames (each sample duplicated on
/// every channel); `out` is overwritten.
pub fn mono_to_bytes(mono: &[f32], channels: usize, out: &mut Vec<u8>) {
    let channels = channels.max(1);
    out.clear();
    out.reserve(mono.len() * channels * 4);
    for &x in mono {
        let b = x.to_le_bytes();
        for _ in 0..channels {
            out.extend_from_slice(&b);
        }
    }
}

/// Loopback endpoints deliver no packets while nothing is playing.  This
/// tracks the time covered by real packets and says how many zeros to
/// synthesise so the stream keeps its nominal rate.
///
/// Invariants (times in seconds on a monotonic clock):
/// * `covered` is the time up to which output exists: real packets set it to
///   their arrival time, synthetic zeros advance it by exactly
///   `frames / FS`.
/// * Zeros are only generated up to `now - grace`, so while real packets
///   flow (they arrive every period, `grace` is several periods) nothing is
///   inserted, and when playback resumes the first real packet does not
///   overlap the zeros already emitted (no double counting).
#[derive(Debug, Clone)]
pub struct IdleFiller {
    grace_s: f64,
    covered: f64,
}

impl IdleFiller {
    pub fn new(now: f64, grace_s: f64) -> Self {
        Self {
            grace_s,
            covered: now,
        }
    }

    /// Real frames arrived at `now`: they cover everything up to `now`.
    pub fn on_packet(&mut self, now: f64) {
        self.covered = self.covered.max(now);
    }

    /// Number of zero frames to emit at `now` (0 while packets flow).
    pub fn due(&mut self, now: f64) -> usize {
        let target = now - self.grace_s;
        if target <= self.covered {
            return 0;
        }
        let frames = ((target - self.covered) * FS as f64).floor() as usize;
        self.covered += frames as f64 / FS as f64;
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn downmix_and_duplicate() {
        let mut bytes = Vec::new();
        for v in [1.0f32, 3.0, -2.0, 0.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut mono = vec![9.0];
        bytes_to_mono(&bytes, 2, false, &mut mono);
        assert_eq!(mono, vec![9.0, 2.0, -1.0]);
        mono.clear();
        bytes_to_mono(&bytes, 1, false, &mut mono);
        assert_eq!(mono, vec![1.0, 3.0, -2.0, 0.0]);
        mono.clear();
        bytes_to_mono(&bytes, 2, true, &mut mono);
        assert_eq!(mono, vec![0.0, 0.0]);
        // partial trailing frame is ignored
        mono.clear();
        bytes_to_mono(&bytes[..12], 2, false, &mut mono);
        assert_eq!(mono, vec![2.0]);

        let mut out = vec![1u8; 3];
        mono_to_bytes(&[0.5, -1.0], 3, &mut out);
        assert_eq!(out.len(), 2 * 3 * 4);
        let mut back = Vec::new();
        bytes_to_mono(&out, 3, false, &mut back);
        assert_eq!(back, vec![0.5, -1.0]);
        let f = |i: usize| f32::from_le_bytes(out[4 * i..4 * i + 4].try_into().unwrap());
        assert_eq!([f(0), f(1), f(2), f(3)], [0.5, 0.5, 0.5, -1.0]);
    }

    #[test]
    fn sizing() {
        assert_eq!(frames_to_hns(480), 100_000);
        assert_eq!(hns_to_frames(100_000), 480);
        assert_eq!(hns_to_frames(100_001), 481);
        assert_eq!(hns_to_frames(-5), 0);
        assert_eq!(render_ring_capacity(480, 480), 960);
        assert_eq!(render_ring_capacity(480, 144), 624);
        assert_eq!(render_ring_capacity(0, 0), 1);
        assert_eq!(capture_ring_capacity(480, 1056), 4 * 480 + 2 * 1056);
        assert_eq!(device_buffer_frames(true, 480, 480), 960);
        assert_eq!(device_buffer_frames(true, 960, 144), 960);
        assert_eq!(device_buffer_frames(false, 480, 480), 4800);
        assert_eq!(render_fill_target(0, 0), 1);
        // target 960, buffer 1056
        assert_eq!(render_frames_now(960, 1056, 0, 5000), 960);
        assert_eq!(render_frames_now(960, 1056, 500, 5000), 460);
        assert_eq!(render_frames_now(960, 1056, 500, 100), 100);
        assert_eq!(render_frames_now(960, 1056, 1000, 5000), 0);
        assert_eq!(render_frames_now(2000, 1056, 1000, 5000), 56);
    }

    #[test]
    fn idle_filler_keeps_nominal_rate() {
        let p = 0.01; // 10 ms period
        let mut f = IdleFiller::new(0.0, 3.0 * p);
        // packets every period: nothing synthesised
        for k in 1..=100 {
            let t = k as f64 * p;
            f.on_packet(t);
            assert_eq!(f.due(t + 0.5 * p), 0);
        }
        // playback stops at t = 1.0; timeouts every period for 2 s
        let mut zeros = 0;
        let mut t = 1.0;
        while t < 3.0 - 1e-9 {
            t += p;
            zeros += f.due(t);
        }
        // covers 1.0 .. (3.0 - grace) within one frame
        let want = ((3.0 - 3.0 * p - 1.0) * FS as f64) as i64;
        assert!((zeros as i64 - want).abs() <= 1, "{zeros} vs {want}");
        // playback resumes: real packet, no extra zeros afterwards
        f.on_packet(t);
        assert_eq!(f.due(t + p), 0);
        // long gap with a single late call: still exact
        let z = f.due(t + 1.0);
        assert!((z as i64 - ((1.0 - 3.0 * p) * FS as f64) as i64).abs() <= 1);
        // fractional bookkeeping does not drift over many small calls
        let mut f = IdleFiller::new(0.0, 0.0);
        let mut total = 0usize;
        for k in 1..=10_000 {
            total += f.due(k as f64 * 0.000_37);
        }
        let want = (10_000.0 * 0.000_37 * FS as f64) as i64;
        assert!((total as i64 - want).abs() <= 1);
    }

    #[test]
    fn blocking_read_waits_for_producer() {
        let (mut prod, mut cons) = RingBuffer::<f32>::new(64);
        let shared = Arc::new(Shared::new());
        let sh = shared.clone();
        let t = thread::spawn(move || {
            for k in 0..10 {
                thread::sleep(Duration::from_millis(2));
                let x: Vec<f32> = (0..30).map(|i| (k * 30 + i) as f32).collect();
                let mut rest = &x[..];
                while !rest.is_empty() {
                    let (_, r) = prod.push_partial_slice(rest);
                    rest = r;
                    sh.notify();
                    thread::yield_now();
                }
            }
            sh.mark_dead();
        });
        let mut got = Vec::new();
        let mut buf = [0.0f32; 50];
        for _ in 0..6 {
            read_blocking(&mut cons, &shared, "mic", &mut buf).unwrap();
            got.extend_from_slice(&buf);
        }
        assert_eq!(got, (0..300).map(|i| i as f32).collect::<Vec<_>>());
        // producer finished: the next read fails instead of blocking forever
        let e = read_blocking(&mut cons, &shared, "mic", &mut buf).unwrap_err();
        assert!(e.to_string().contains("mic"));
        t.join().unwrap();
    }

    #[test]
    fn blocking_write_waits_for_space_and_reports_errors() {
        let (mut prod, mut cons) = RingBuffer::<f32>::new(16);
        let shared = Arc::new(Shared::new());
        let sh = shared.clone();
        let t = thread::spawn(move || {
            let mut got = Vec::new();
            let mut buf = [0.0f32; 8];
            while got.len() < 100 {
                let n = cons.pop_partial_slice(&mut buf).0.len();
                got.extend_from_slice(&buf[..n]);
                sh.notify();
                thread::sleep(Duration::from_millis(1));
            }
            sh.fail("device invalidated".into());
            got
        });
        let x: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let t0 = Instant::now();
        write_blocking(&mut prod, &shared, "speaker", &x).unwrap();
        assert!(t0.elapsed() > Duration::from_millis(3)); // had to wait for space
        let got = t.join().unwrap();
        assert_eq!(got, x);
        let e = write_blocking(&mut prod, &shared, "speaker", &[1.0]).unwrap_err();
        assert!(e.to_string().contains("device invalidated"), "{e}");
    }

    #[test]
    fn push_or_drop_counts_overflow() {
        let (mut prod, _cons) = RingBuffer::<f32>::new(4);
        assert_eq!(push_or_drop(&mut prod, &[1.0, 2.0, 3.0]), 0);
        assert_eq!(push_or_drop(&mut prod, &[4.0, 5.0, 6.0]), 2);
    }

    #[test]
    fn stop_flag() {
        let s = Shared::new();
        assert!(!s.stop_requested());
        s.request_stop();
        assert!(s.stop_requested());
        assert!(!s.is_dead());
        // waiting on a dead stream returns at once
        s.mark_dead();
        let t0 = Instant::now();
        s.wait_while(|| true);
        assert!(t0.elapsed() < Duration::from_millis(50));
    }
}
