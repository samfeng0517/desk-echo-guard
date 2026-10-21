//! WASAPI backend (shared mode, event driven, loopback capture).
//!
//! Every stream owns one device thread.  The thread initialises COM (MTA),
//! opens the endpoint, then sleeps on the WASAPI event handle and moves data
//! between the device and a lock-free SPSC ring (`rtrb`).  The pipeline side
//! (`read` / `write`) blocks on that ring through a condvar (`stream.rs`);
//! nothing polls.
//!
//! COM objects never leave the thread that created them; the stream handle
//! only holds the ring end, the shared state and the join handle.

use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use rtrb::{Consumer, Producer, RingBuffer};
use wasapi::{
    AudioClient, Device, DeviceEnumerator, Direction, Handle, SampleType, StreamMode, WasapiError,
    WaveFormat,
};

use crate::stream::{
    bytes_to_mono, capture_ring_capacity, device_buffer_frames, frames_to_hns, hns_to_frames,
    mono_to_bytes, push_or_drop, read_blocking, render_fill_target, render_frames_now,
    render_ring_capacity, write_blocking, IdleFiller, Shared, FS,
};
use crate::{match_device, AudioBackend, DeviceInfo, DeviceKind, InputStream, OutputStream};

/// How long `open_*` waits for the device thread to come up.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// Event wait of non-loopback streams.  Events normally arrive every device
/// period; a timeout only lets the thread notice `stop` and probe the device
/// (the next WASAPI call reports invalidation).
const EVENT_TIMEOUT_MS: u32 = 200;
/// Idle loopback: zeros start this many device periods after the last real
/// packet (see `IdleFiller`).
const IDLE_GRACE_PERIODS: f64 = 3.0;
/// AUDCLNT_E_DEVICE_INVALIDATED
const DEVICE_INVALIDATED: u32 = 0x8889_0004;

/// COM for the current thread (MTA); uninitialised on drop only if the
/// init succeeded (S_OK and S_FALSE both need a matching uninit).
struct ComGuard(bool);

impl ComGuard {
    fn init_mta() -> Self {
        ComGuard(wasapi::initialize_mta().is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            wasapi::deinitialize();
        }
    }
}

/// MMCSS "Pro Audio" scheduling for a device thread; reverted on drop.
struct MmcssGuard(windows_sys::Win32::Foundation::HANDLE);

impl MmcssGuard {
    fn join() -> Self {
        use windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the
        // call; `index` is a valid out pointer.  Failure returns null.
        let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
        MmcssGuard(h)
    }
}

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics;
        if !self.0.is_null() {
            // SAFETY: handle returned by AvSetMmThreadCharacteristicsW on
            // this thread, reverted once.
            unsafe { AvRevertMmThreadCharacteristics(self.0) };
        }
    }
}

/// Run `f` on a short-lived thread with its own MTA, so the caller's
/// apartment (e.g. an STA GUI thread) does not matter.
fn with_mta<T: Send>(f: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    std::thread::scope(|s| {
        s.spawn(|| {
            let _com = ComGuard::init_mta();
            f()
        })
        .join()
        .map_err(|_| anyhow!("WASAPI helper thread panicked"))?
    })
}

/// Readable text for a device-thread error.
fn describe(e: &anyhow::Error) -> String {
    if let Some(WasapiError::Windows(w)) = e.downcast_ref::<WasapiError>() {
        if w.code().0 as u32 == DEVICE_INVALIDATED {
            return "audio device was removed or disabled (AUDCLNT_E_DEVICE_INVALIDATED)".into();
        }
    }
    format!("{e:#}")
}

fn kind_of(dir: Direction) -> DeviceKind {
    match dir {
        Direction::Render => DeviceKind::Speaker,
        Direction::Capture => DeviceKind::Mic,
    }
}

fn device_channels(dev: &Device) -> u16 {
    dev.get_device_format()
        .map(|f| f.get_nchannels())
        .or_else(|_| {
            dev.get_iaudioclient()
                .and_then(|c| c.get_mixformat())
                .map(|f| f.get_nchannels())
        })
        .unwrap_or(2)
}

/// Active endpoints of one direction.  A device that fails to report its
/// properties is skipped rather than failing the whole list.
fn enumerate(en: &DeviceEnumerator, dir: Direction) -> Result<Vec<DeviceInfo>> {
    let default_id = en.get_default_device(&dir).and_then(|d| d.get_id()).ok();
    let coll = en.get_device_collection(&dir)?;
    let mut out = Vec::new();
    for i in 0..coll.get_nbr_devices()? {
        let Ok(dev) = coll.get_device_at_index(i) else {
            continue;
        };
        let Ok(id) = dev.get_id() else { continue };
        let name = dev.get_friendlyname().unwrap_or_else(|_| id.clone());
        out.push(DeviceInfo {
            kind: kind_of(dir),
            default: default_id.as_deref() == Some(id.as_str()),
            channels: device_channels(&dev),
            name,
            id,
        });
    }
    Ok(out)
}

/// `None` = default endpoint of `dir`; else `match_device` on its endpoints.
fn resolve(
    en: &DeviceEnumerator,
    dir: Direction,
    name: Option<&str>,
    what: &str,
) -> Result<(Device, String)> {
    let dev = match name {
        None => en
            .get_default_device(&dir)
            .with_context(|| format!("no default {what}"))?,
        Some(q) => {
            let list = enumerate(en, dir)?;
            let d = match_device(&list, q, what)?;
            en.get_device(&d.id)?
        }
    };
    let label = dev.get_friendlyname().unwrap_or_else(|_| "?".into());
    Ok((dev, label))
}

/// An initialised shared-mode, event-driven client.
struct Opened {
    client: AudioClient,
    event: Handle,
    channels: usize,
    block_align: usize,
    buffer_frames: usize,
    period_frames: usize,
}

/// Open `dev` for `stream_dir` (Capture on a render endpoint = loopback) as
/// 48 kHz f32 with the device's own channel count/mask.  Windows converts
/// (AUTOCONVERTPCM | SRC_DEFAULT_QUALITY) only if the mix format differs.
fn open_client(dev: &Device, stream_dir: Direction, blocksize: usize) -> Result<Opened> {
    let mut client = dev.get_iaudioclient()?;
    let mix = client.get_mixformat()?;
    let channels = mix.get_nchannels().max(1) as usize;
    let fmt = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        FS,
        channels,
        Some(mix.get_dwchannelmask()),
    );
    let native = mix.get_samplespersec() as usize == FS
        && mix.get_bitspersample() == 32
        && matches!(mix.get_subformat(), Ok(SampleType::Float));
    let (default_period, _min_period) = client.get_device_period()?;
    let period_frames = hns_to_frames(default_period).max(1);
    let render = stream_dir == Direction::Render;
    let want = device_buffer_frames(render, blocksize, period_frames);
    let mode = StreamMode::EventsShared {
        autoconvert: !native,
        buffer_duration_hns: frames_to_hns(want),
    };
    client.initialize_client(&fmt, &stream_dir, &mode)?;
    let event = client.set_get_eventhandle()?;
    let buffer_frames = client.get_buffer_size()? as usize;
    Ok(Opened {
        client,
        event,
        channels,
        block_align: fmt.get_blockalign() as usize,
        buffer_frames,
        period_frames,
    })
}

type InitTx<T> = SyncSender<std::result::Result<T, String>>;

/// Spawn a device thread and wait until it reports that the stream runs.
/// `body` must send its ring end through the init channel once the device
/// is started, then loop until `shared.stop_requested()` or an error.
fn spawn_device_thread<T: Send + 'static>(
    thread_name: String,
    shared: Arc<Shared>,
    body: impl FnOnce(&Shared, &mut Option<InitTx<T>>) -> Result<()> + Send + 'static,
) -> Result<(T, JoinHandle<()>)> {
    let (tx, rx) = sync_channel(1);
    let sh = shared.clone();
    let handle = std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let com = ComGuard::init_mta();
            let mmcss = MmcssGuard::join();
            let mut init = Some(tx);
            // All COM objects live inside `body` and are released when it
            // returns, before COM is uninitialised below.
            if let Err(e) = body(&sh, &mut init) {
                let msg = describe(&e);
                if let Some(tx) = init.take() {
                    let _ = tx.send(Err(msg.clone()));
                }
                sh.fail(msg);
            }
            drop(mmcss);
            drop(com);
            sh.mark_dead();
        })?;
    match rx.recv_timeout(INIT_TIMEOUT) {
        Ok(Ok(t)) => Ok((t, handle)),
        Ok(Err(e)) => {
            let _ = handle.join();
            bail!(e)
        }
        Err(_) => {
            // Thread hung in device setup (or died): let it exit on its own.
            shared.request_stop();
            bail!("audio device did not start within {INIT_TIMEOUT:?}")
        }
    }
}

/// Capture thread body (microphone, or loopback of a render endpoint).
fn capture_body(
    name: Option<String>,
    loopback: bool,
    blocksize: usize,
    shared: &Shared,
    init: &mut Option<InitTx<(Consumer<f32>, String)>>,
) -> Result<()> {
    let en = DeviceEnumerator::new()?;
    let (dev_dir, what) = if loopback {
        (Direction::Render, "speaker")
    } else {
        (Direction::Capture, "microphone")
    };
    let (dev, label) = resolve(&en, dev_dir, name.as_deref(), what)?;
    let o = open_client(&dev, Direction::Capture, blocksize)?;
    let capture = o.client.get_audiocaptureclient()?;
    let cap = capture_ring_capacity(blocksize, o.buffer_frames);
    let (mut prod, cons): (Producer<f32>, _) = RingBuffer::new(cap);

    let mut bytes = vec![0u8; o.buffer_frames.max(1) * o.block_align];
    let mut mono: Vec<f32> = Vec::with_capacity(cap);
    let period_s = o.period_frames as f64 / FS as f64;
    let timeout_ms = if loopback {
        // wake at least once per period to synthesise idle silence
        ((period_s * 1000.0).ceil() as u32).max(1)
    } else {
        EVENT_TIMEOUT_MS
    };
    let t0 = Instant::now();
    let now = || t0.elapsed().as_secs_f64();
    let mut idle = loopback.then(|| IdleFiller::new(now(), IDLE_GRACE_PERIODS * period_s));

    o.client.start_stream()?;
    if let Some(tx) = init.take() {
        let _ = tx.send(Ok((cons, label)));
    }

    while !shared.stop_requested() {
        // Err = timeout; either way drain whatever is there.  The packet
        // size query also surfaces device invalidation after a timeout.
        let _ = o.event.wait_for_event(timeout_ms);
        mono.clear();
        let mut real = false;
        loop {
            match capture.get_next_packet_size()? {
                Some(0) => break,
                // shared mode always reports a size; be defensive
                None => break,
                Some(_) => {}
            }
            let (frames, info) = capture.read_from_device(&mut bytes)?;
            if frames == 0 {
                break;
            }
            let n = frames as usize * o.block_align;
            bytes_to_mono(&bytes[..n], o.channels, info.flags.silent, &mut mono);
            real = true;
        }
        if let Some(f) = idle.as_mut() {
            let t = now();
            if real {
                f.on_packet(t);
            }
            // capped: more than the ring holds would be dropped anyway
            let z = f.due(t).min(cap);
            mono.resize(mono.len() + z, 0.0);
        }
        if !mono.is_empty() {
            // Overflow (reader stalled) drops the newest samples; the
            // pipeline's elastic buffers recover from the gap.
            let _dropped = push_or_drop(&mut prod, &mono);
            shared.notify();
        }
    }
    let _ = o.client.stop_stream();
    Ok(())
}

/// Render thread body.
fn render_body(
    name: Option<String>,
    blocksize: usize,
    shared: &Shared,
    init: &mut Option<InitTx<(Producer<f32>, String)>>,
) -> Result<()> {
    let en = DeviceEnumerator::new()?;
    let (dev, label) = resolve(&en, Direction::Render, name.as_deref(), "speaker")?;
    let o = open_client(&dev, Direction::Render, blocksize)?;
    let render = o.client.get_audiorenderclient()?;
    let (prod, mut cons): (_, Consumer<f32>) =
        RingBuffer::new(render_ring_capacity(blocksize, o.period_frames));
    let target = render_fill_target(blocksize, o.period_frames).min(o.buffer_frames);
    let mut mono = vec![0.0f32; o.buffer_frames];
    let mut bytes: Vec<u8> = Vec::with_capacity(o.buffer_frames * o.block_align);

    // Starting with an empty buffer is fine: the engine plays silence until
    // the first block arrives, and that keeps start-up latency minimal.
    o.client.start_stream()?;
    if let Some(tx) = init.take() {
        let _ = tx.send(Ok((prod, label)));
    }

    while !shared.stop_requested() {
        let _ = o.event.wait_for_event(EVENT_TIMEOUT_MS);
        // also the invalidation probe after a timeout
        let padding = o.client.get_current_padding()? as usize;
        let n = render_frames_now(target, o.buffer_frames, padding, cons.slots());
        if n == 0 {
            // Ring empty: the engine inserts silence (an audible glitch, as
            // with any underrun) and we resume as soon as data is queued.
            continue;
        }
        let popped = cons.pop_partial_slice(&mut mono[..n]).0.len();
        shared.notify();
        mono_to_bytes(&mono[..popped], o.channels, &mut bytes);
        render.write_to_device(popped, &bytes, None)?;
    }
    let _ = o.client.stop_stream();
    Ok(())
}

/// Capture stream handle (microphone or loopback).
pub struct WasapiInput {
    cons: Consumer<f32>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    what: String,
}

impl WasapiInput {
    /// Friendly name of the opened endpoint.
    pub fn device_name(&self) -> &str {
        &self.what
    }
}

impl InputStream for WasapiInput {
    fn read(&mut self, out: &mut [f32]) -> Result<()> {
        read_blocking(&mut self.cons, &self.shared, &self.what, out)
    }
}

impl Drop for WasapiInput {
    fn drop(&mut self) {
        self.shared.request_stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Render stream handle.
pub struct WasapiOutput {
    prod: Producer<f32>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    what: String,
}

impl WasapiOutput {
    /// Friendly name of the opened endpoint.
    pub fn device_name(&self) -> &str {
        &self.what
    }
}

impl OutputStream for WasapiOutput {
    fn write(&mut self, x: &[f32]) -> Result<()> {
        write_blocking(&mut self.prod, &self.shared, &self.what, x)
    }
}

impl Drop for WasapiOutput {
    fn drop(&mut self) {
        self.shared.request_stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Real devices through WASAPI shared mode.
pub struct WasapiBackend {
    _priv: (),
}

impl WasapiBackend {
    /// Checks that the audio service can be reached.
    pub fn new() -> Result<Self> {
        with_mta(|| {
            DeviceEnumerator::new().context("Windows audio service not available")?;
            Ok(())
        })?;
        Ok(Self { _priv: () })
    }
}

impl AudioBackend for WasapiBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        with_mta(|| {
            let en = DeviceEnumerator::new()?;
            let mut v = enumerate(&en, Direction::Capture)?;
            v.extend(enumerate(&en, Direction::Render)?);
            Ok(v)
        })
    }

    fn open_input(
        &self,
        name: Option<&str>,
        loopback: bool,
        blocksize: usize,
    ) -> Result<Box<dyn InputStream>> {
        let shared = Arc::new(Shared::new());
        let name = name.map(str::to_owned);
        let tname = if loopback {
            "wasapi-loopback"
        } else {
            "wasapi-capture"
        };
        let ((cons, label), thread) =
            spawn_device_thread(tname.into(), shared.clone(), move |sh, init| {
                capture_body(name, loopback, blocksize, sh, init)
            })?;
        let what = if loopback {
            format!("loopback '{label}'")
        } else {
            format!("microphone '{label}'")
        };
        Ok(Box::new(WasapiInput {
            cons,
            shared,
            thread: Some(thread),
            what,
        }))
    }

    fn open_output(&self, name: Option<&str>, blocksize: usize) -> Result<Box<dyn OutputStream>> {
        let shared = Arc::new(Shared::new());
        let name = name.map(str::to_owned);
        let ((prod, label), thread) =
            spawn_device_thread("wasapi-render".into(), shared.clone(), move |sh, init| {
                render_body(name, blocksize, sh, init)
            })?;
        Ok(Box::new(WasapiOutput {
            prod,
            shared,
            thread: Some(thread),
            what: format!("speaker '{label}'"),
        }))
    }
}
