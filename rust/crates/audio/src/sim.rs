//! Offline / simulated audio backend (`sim.py`).
//!
//! [`SimBackend`] serves pre-computed signals as "devices", paced in (scaled)
//! real time by each stream's own clock, so the threaded pipeline, the
//! elastic buffers and clock-drift handling can be exercised without any
//! hardware.  Every simulated device can run at its own clock rate (ppm
//! offset), like real sound cards.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use parking_lot::Mutex;

use crate::{
    match_device, AudioBackend, DeviceInfo, DeviceKind, InputStream, OutputStream,
};

const FS: f64 = 48_000.0;

/// Paces a stream: n samples take n / (FS * (1 + ppm)) / speed seconds.
#[derive(Debug)]
struct SimClock {
    rate: f64,
    t0: Option<Instant>,
    count: u64,
}

impl SimClock {
    fn new(ppm: f64, speed: f64) -> Self {
        Self {
            rate: FS * (1.0 + ppm * 1e-6) * speed,
            t0: None,
            count: 0,
        }
    }

    /// Account for `n` samples and sleep until they are due.
    fn wait(&mut self, n: usize) {
        let t0 = *self.t0.get_or_insert_with(Instant::now);
        self.count += n as u64;
        let due = t0 + Duration::from_secs_f64(self.count as f64 / self.rate);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
    }
}

/// Simulated capture stream: plays back a fixed signal, zeros after its end
/// (or wraps around when looping).
pub struct SimInput {
    x: Arc<[f32]>,
    pos: usize,
    looping: bool,
    clock: SimClock,
}

impl SimInput {
    pub fn new(signal: Arc<[f32]>, ppm: f64, speed: f64, looping: bool) -> Self {
        Self {
            x: signal,
            pos: 0,
            looping,
            clock: SimClock::new(ppm, speed),
        }
    }

    /// Fill `out` from the current position without pacing.
    fn fill(&mut self, out: &mut [f32]) {
        let len = self.x.len();
        for (i, o) in out.iter_mut().enumerate() {
            let p = self.pos + i;
            *o = if p < len {
                self.x[p]
            } else if self.looping && len > 0 {
                self.x[p % len]
            } else {
                0.0
            };
        }
        self.pos += out.len();
    }
}

impl InputStream for SimInput {
    fn read(&mut self, out: &mut [f32]) -> Result<()> {
        self.clock.wait(out.len());
        self.fill(out);
        Ok(())
    }
}

/// Simulated render stream: records everything written to it.
pub struct SimOutput {
    clock: SimClock,
    data: Arc<Mutex<Vec<f32>>>,
}

impl SimOutput {
    pub fn new(ppm: f64, speed: f64) -> Self {
        Self {
            clock: SimClock::new(ppm, speed),
            data: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Everything written so far.
    pub fn data(&self) -> Vec<f32> {
        self.data.lock().clone()
    }

    /// Shared handle to the recording (stays valid after the stream is dropped).
    pub fn recording(&self) -> Arc<Mutex<Vec<f32>>> {
        self.data.clone()
    }
}

impl OutputStream for SimOutput {
    fn write(&mut self, x: &[f32]) -> Result<()> {
        self.clock.wait(x.len());
        self.data.lock().extend_from_slice(x);
        Ok(())
    }
}

#[derive(Clone)]
struct Source {
    name: String,
    signal: Arc<[f32]>,
    ppm: f64,
    looping: bool,
}

#[derive(Default)]
struct Devices {
    inputs: Vec<Source>,
    loopbacks: Vec<Source>,
    /// (name, ppm)
    outputs: Vec<(String, f64)>,
    /// recordings of opened outputs, keyed by resolved device name
    opened: HashMap<String, Arc<Mutex<Vec<f32>>>>,
}

/// Simulated devices.  Mics come from `inputs`, loopback capture of a
/// speaker from `loopbacks` (keyed by speaker name), speakers from
/// `outputs`.  Devices may be added at any time (`&self`), e.g. to simulate
/// hot-plugging while a GUI is running.
pub struct SimBackend {
    speed: f64,
    devs: Mutex<Devices>,
}

impl Default for SimBackend {
    fn default() -> Self {
        Self::new(1.0)
    }
}

fn upsert_source(list: &mut Vec<Source>, src: Source) {
    match list.iter_mut().find(|s| s.name == src.name) {
        Some(s) => *s = src,
        None => list.push(src),
    }
}

impl SimBackend {
    /// `speed` > 1 runs all streams faster than real time.
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            devs: Mutex::new(Devices::default()),
        }
    }

    /// Add (or replace) a microphone.
    pub fn add_input(&self, name: &str, signal: impl Into<Arc<[f32]>>, ppm: f64, looping: bool) {
        let src = Source {
            name: name.into(),
            signal: signal.into(),
            ppm,
            looping,
        };
        upsert_source(&mut self.devs.lock().inputs, src);
    }

    /// Add (or replace) the loopback signal of speaker `name`.
    pub fn add_loopback(
        &self,
        name: &str,
        signal: impl Into<Arc<[f32]>>,
        ppm: f64,
        looping: bool,
    ) {
        let src = Source {
            name: name.into(),
            signal: signal.into(),
            ppm,
            looping,
        };
        upsert_source(&mut self.devs.lock().loopbacks, src);
    }

    /// Add (or replace) a speaker.
    pub fn add_output(&self, name: &str, ppm: f64) {
        let mut d = self.devs.lock();
        match d.outputs.iter_mut().find(|(n, _)| n == name) {
            Some(o) => o.1 = ppm,
            None => d.outputs.push((name.into(), ppm)),
        }
    }

    /// Builder form of [`add_input`](Self::add_input) (not looping).
    pub fn with_input(self, name: &str, signal: impl Into<Arc<[f32]>>, ppm: f64) -> Self {
        self.add_input(name, signal, ppm, false);
        self
    }

    /// Builder form of [`add_loopback`](Self::add_loopback) (not looping).
    pub fn with_loopback(self, name: &str, signal: impl Into<Arc<[f32]>>, ppm: f64) -> Self {
        self.add_loopback(name, signal, ppm, false);
        self
    }

    /// Builder form of [`add_output`](Self::add_output).
    pub fn with_output(self, name: &str, ppm: f64) -> Self {
        self.add_output(name, ppm);
        self
    }

    /// Everything written to the opened output `name` (exact name, else
    /// substring as in [`match_device`]); empty if it was never opened.
    pub fn output_data(&self, name: &str) -> Vec<f32> {
        let d = self.devs.lock();
        if let Some(rec) = d.opened.get(name) {
            return rec.lock().clone();
        }
        let items: Vec<DeviceInfo> = d.opened.keys().map(|k| info(DeviceKind::Speaker, k, 2)).collect();
        match match_device(&items, name, "speaker") {
            Ok(dev) => d.opened[&dev.name].lock().clone(),
            Err(_) => Vec::new(),
        }
    }

    /// Names of the outputs opened so far.
    pub fn opened_outputs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.devs.lock().opened.keys().cloned().collect();
        v.sort();
        v
    }
}

fn info(kind: DeviceKind, name: &str, channels: u16) -> DeviceInfo {
    DeviceInfo {
        kind,
        name: name.into(),
        id: name.into(),
        channels,
        default: false,
    }
}

/// `None` = first device; otherwise [`match_device`] on the names.
fn find<'a, T>(
    items: &'a [T],
    name_of: impl Fn(&T) -> &str,
    query: Option<&str>,
    what: &str,
) -> Result<&'a T> {
    let Some(q) = query else {
        return items.first().ok_or_else(|| anyhow!("no {what} available"));
    };
    let infos: Vec<DeviceInfo> = items
        .iter()
        .map(|t| info(DeviceKind::Mic, name_of(t), 1))
        .collect();
    let hit = match_device(&infos, q, what)?;
    let idx = infos
        .iter()
        .position(|d| std::ptr::eq(d, hit))
        .ok_or_else(|| anyhow!("internal: device lookup"))?;
    Ok(&items[idx])
}

impl AudioBackend for SimBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let d = self.devs.lock();
        let mut v: Vec<DeviceInfo> = d
            .inputs
            .iter()
            .map(|s| info(DeviceKind::Mic, &s.name, 1))
            .collect();
        v.extend(d.outputs.iter().map(|(n, _)| info(DeviceKind::Speaker, n, 2)));
        Ok(v)
    }

    fn open_input(
        &self,
        name: Option<&str>,
        loopback: bool,
        _blocksize: usize,
    ) -> Result<Box<dyn InputStream>> {
        let d = self.devs.lock();
        let (list, what) = if loopback {
            (&d.loopbacks, "speaker")
        } else {
            (&d.inputs, "microphone")
        };
        let s = find(list, |s| s.name.as_str(), name, what)?;
        Ok(Box::new(SimInput::new(
            s.signal.clone(),
            s.ppm,
            self.speed,
            s.looping,
        )))
    }

    fn open_output(&self, name: Option<&str>, _blocksize: usize) -> Result<Box<dyn OutputStream>> {
        let mut d = self.devs.lock();
        let (key, ppm) = find(&d.outputs, |o| o.0.as_str(), name, "speaker")?.clone();
        let out = SimOutput::new(ppm, self.speed);
        d.opened.insert(key, out.recording());
        Ok(Box::new(out))
    }

    fn speed(&self) -> f64 {
        self.speed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    #[test]
    fn input_plays_signal_then_zeros() {
        let mut s = SimInput::new(ramp(5).into(), 0.0, 1000.0, false);
        let mut out = [9.0f32; 3];
        s.read(&mut out).unwrap();
        assert_eq!(out, [0.0, 1.0, 2.0]);
        s.read(&mut out).unwrap();
        assert_eq!(out, [3.0, 4.0, 0.0]);
        s.read(&mut out).unwrap();
        assert_eq!(out, [0.0; 3]);
    }

    #[test]
    fn input_loops() {
        let mut s = SimInput::new(ramp(4).into(), 0.0, 1000.0, true);
        let mut out = [0.0f32; 6];
        s.read(&mut out).unwrap();
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0, 0.0, 1.0]);
        s.read(&mut out).unwrap();
        assert_eq!(out, [2.0, 3.0, 0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn empty_looping_input_is_silent() {
        let mut s = SimInput::new(Vec::new().into(), 0.0, 1000.0, true);
        let mut out = [1.0f32; 4];
        s.read(&mut out).unwrap();
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn clock_paces_with_speed_and_ppm() {
        // 4800 samples at speed 2 -> 50 ms
        let mut s = SimInput::new(ramp(10).into(), 0.0, 2.0, false);
        let mut out = vec![0.0f32; 480];
        let t = Instant::now();
        for _ in 0..10 {
            s.read(&mut out).unwrap();
        }
        let dt = t.elapsed().as_secs_f64();
        assert!((0.045..0.2).contains(&dt), "dt = {dt}");
        // a fast clock (+ppm) consumes samples at a higher rate
        let c = SimClock::new(1e5, 1.0);
        assert!((c.rate - 48_000.0 * 1.1).abs() < 1e-6);
    }

    #[test]
    fn backend_lists_and_finds_devices() {
        let be = SimBackend::new(1000.0)
            .with_input("USB Mic", ramp(10), 0.0)
            .with_input("CABLE-A Output", ramp(10), -40.0)
            .with_loopback("Speakers (Realtek)", vec![7.0f32; 10], 0.0)
            .with_output("CABLE Input", -60.0)
            .with_output("Speakers (Realtek)", 0.0);
        let devs = be.list_devices().unwrap();
        assert_eq!(devs.len(), 4);
        assert_eq!(devs[0].kind, DeviceKind::Mic);
        assert_eq!(devs[0].channels, 1);
        assert_eq!(devs[3].kind, DeviceKind::Speaker);
        assert_eq!(devs[3].channels, 2);

        let mut lb = be.open_input(Some("realtek"), true, 480).unwrap();
        let mut x = [0.0f32; 2];
        lb.read(&mut x).unwrap();
        assert_eq!(x, [7.0, 7.0]);
        let mut mic = be.open_input(None, false, 480).unwrap();
        mic.read(&mut x).unwrap();
        assert_eq!(x, [0.0, 1.0]);
        assert!(be.open_input(Some("nope"), false, 480).is_err());
        assert!(be.open_input(Some("usb"), true, 480).is_err());

        let mut o = be.open_output(Some("cable input"), 480).unwrap();
        o.write(&[1.0, 2.0]).unwrap();
        o.write(&[3.0]).unwrap();
        drop(o);
        assert_eq!(be.output_data("CABLE Input"), vec![1.0, 2.0, 3.0]);
        assert_eq!(be.output_data("cable"), vec![1.0, 2.0, 3.0]);
        assert!(be.output_data("Speakers (Realtek)").is_empty());
        assert_eq!(be.opened_outputs(), vec!["CABLE Input".to_string()]);
        assert_eq!(be.speed(), 1000.0);
    }

    #[test]
    fn devices_can_be_added_later() {
        let be = SimBackend::new(1000.0).with_output("Speakers", 0.0);
        assert!(be.open_input(None, false, 480).is_err());
        be.add_input("CABLE Output (VB-Audio Virtual Cable)", vec![0.0f32; 480], 0.0, false);
        be.add_output("CABLE Input (VB-Audio Virtual Cable)", 0.0);
        assert_eq!(be.list_devices().unwrap().len(), 3);
        assert!(be.open_input(Some("cable output"), false, 480).is_ok());
        // reopening replaces the recording
        be.open_output(Some("speakers"), 480).unwrap().write(&[1.0]).unwrap();
        be.open_output(Some("speakers"), 480).unwrap().write(&[2.0]).unwrap();
        assert_eq!(be.output_data("Speakers"), vec![2.0]);
    }

    #[test]
    fn works_as_trait_object() {
        let be: Arc<dyn AudioBackend> = Arc::new(SimBackend::new(4.0).with_output("X", 0.0));
        assert_eq!(be.speed(), 4.0);
        assert!(be.open_output(None, 480).is_ok());
    }
}
