//! Voice typing (Typeless, ...): do not send the dictated words to the call.
//!
//! While a voice typing program records from a microphone, the virtual mic
//! (what Discord / LINE hear) is faded to silence, and faded back in shortly
//! after the program lets go of the microphone.  The program is recognised
//! by its recording session, not by its hotkey, so "hold to talk" and "press
//! once to start, again to stop" both work and the hotkey can be anything.
//!
//! [`MuteControl`] (session activity -> muted, with a short hold) and
//! [`MuteGate`] (click-free fade) are platform independent;
//! [`open_watcher`] gives the Windows side (who is recording right now).

use anyhow::Result;

/// Default for `dictation_apps`.
pub const DEFAULT_APPS: &str = "Typeless";
/// How often the recording sessions are looked at.
pub const POLL_S: f64 = 0.05;
/// Stay muted this long after the program stopped recording: the tail of
/// the last word is still in the buffers on its way to the call.
pub const HOLD_S: f64 = 0.25;
/// Fade time of the virtual mic (no click).
const FADE_S: f64 = 0.01;

/// Program names from the setting: separated by `,` `;` or line breaks,
/// lower case, without a path or `.exe`.
pub fn parse_apps(list: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in list.split([',', ';', '\n', '，', '；', '、']) {
        let name = exe_stem(part);
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// "C:\Apps\Typeless.EXE" -> "typeless".
fn exe_stem(path: &str) -> String {
    let name = path.trim().trim_matches('"').trim();
    let name = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).trim().into()
}

/// True if the program `exe` (a file name or full path) is one of `apps`
/// (from [`parse_apps`]): its name contains one of them, so "Typeless" also
/// covers "Typeless Helper.exe".
pub fn app_matches(exe: &str, apps: &[String]) -> bool {
    let stem = exe_stem(exe);
    !stem.is_empty() && apps.iter().any(|a| stem.contains(a.as_str()))
}

/// Recording activity -> "the virtual mic is muted".
#[derive(Debug, Default)]
pub struct MuteControl {
    /// seconds since the program was last seen recording
    idle_s: Option<f64>,
}

impl MuteControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance by `dt` seconds; `recording`: a voice typing program is
    /// recording right now.  Returns whether the virtual mic is muted.
    pub fn step(&mut self, dt: f64, recording: bool) -> bool {
        self.idle_s = match (recording, self.idle_s) {
            (true, _) => Some(0.0),
            (false, Some(t)) if t + dt < HOLD_S => Some(t + dt),
            (false, _) => None,
        };
        self.idle_s.is_some()
    }
}

/// Fades a signal out and back in (linear ramp over [`FADE_S`]).
#[derive(Debug)]
pub struct MuteGate {
    gain: f32,
    step: f32,
}

impl MuteGate {
    pub fn new(fs: usize) -> Self {
        Self {
            gain: 1.0,
            step: (1.0 / (FADE_S * fs as f64).max(1.0)) as f32,
        }
    }

    /// Apply the gate to `x` in place.
    pub fn process(&mut self, muted: bool, x: &mut [f32]) {
        let target = if muted { 0.0 } else { 1.0 };
        if self.gain == target {
            if muted {
                x.fill(0.0);
            }
            return;
        }
        for v in x {
            self.gain = if muted {
                (self.gain - self.step).max(0.0)
            } else {
                (self.gain + self.step).min(1.0)
            };
            *v *= self.gain;
        }
    }
}

/// The Windows side: is one of the voice typing programs recording?
pub trait DictationWatcher: Send {
    /// File name of a watched program that is recording from a microphone
    /// right now, if any.
    fn recording(&mut self) -> Result<Option<String>>;
}

/// Creates the watcher on the thread that uses it (COM).
pub type WatcherFactory = Box<dyn FnOnce() -> Result<Box<dyn DictationWatcher>> + Send>;

/// Watcher for the programs in `apps` (the `dictation_apps` setting).  Must
/// be called, used and dropped on one thread.  Errors on other platforms.
pub fn open_watcher(apps: &str) -> Result<Box<dyn DictationWatcher>> {
    let apps = parse_apps(apps);
    anyhow::ensure!(!apps.is_empty(), "no voice typing program is set");
    imp::open(apps)
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn open(_apps: Vec<String>) -> Result<Box<dyn DictationWatcher>> {
        anyhow::bail!("noticing voice typing needs the Windows audio sessions")
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use anyhow::Context;
    use windows::core::{Interface, HSTRING, PWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use crate::vcable;
    use echoguard_audio::DeviceKind;

    /// The microphone list is read again this often (devices come and go).
    const RELIST: Duration = Duration::from_secs(3);

    pub struct SessionWatcher {
        enumerator: IMMDeviceEnumerator,
        apps: Vec<String>,
        own_pid: u32,
        /// every active microphone except the VB-Cable ones
        mics: Vec<IMMDevice>,
        listed: Option<Instant>,
        /// process id -> file name ("" = cannot be read)
        names: HashMap<u32, String>,
    }

    // SAFETY: the COM objects live in the multithreaded apartment and are
    // free-threaded; the watcher is still created, used and dropped by the
    // one control thread.
    unsafe impl Send for SessionWatcher {}

    pub fn open(apps: Vec<String>) -> Result<Box<dyn DictationWatcher>> {
        // SAFETY: plain COM initialisation / activation; the interface is
        // owned by the returned value
        unsafe {
            // never balanced with CoUninitialize: the control thread ends
            // right after dropping the watcher, and its interfaces must not
            // outlive the apartment
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("CoCreateInstance(MMDeviceEnumerator) failed")?;
            let mut w = SessionWatcher {
                enumerator,
                apps,
                own_pid: std::process::id(),
                mics: Vec::new(),
                listed: None,
                names: HashMap::new(),
            };
            w.relist()?;
            Ok(Box::new(w))
        }
    }

    /// File name of a process ("" if it cannot be read, e.g. it has ended).
    fn process_name(pid: u32) -> String {
        // SAFETY: the handle is closed again; the buffer outlives the call
        // and its length is passed in
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return String::new();
            };
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(handle);
            if !ok {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit(['\\', '/']).next().unwrap_or("").to_string()
        }
    }

    impl SessionWatcher {
        /// The microphones to watch.  The VB-Cable ones are left out: what
        /// records there is the call program (or a voice typing program
        /// that would then hear the muted signal itself).
        fn relist(&mut self) -> Result<()> {
            let mut mics = Vec::new();
            for ep in vcable::list_endpoints()? {
                if ep.kind != DeviceKind::Mic
                    || ep.state != vcable::STATE_ACTIVE
                    || vcable::is_vbcable(&ep.name)
                {
                    continue;
                }
                // SAFETY: COM call on a valid enumerator
                if let Ok(dev) =
                    unsafe { self.enumerator.GetDevice(&HSTRING::from(ep.id.as_str())) }
                {
                    mics.push(dev);
                }
            }
            self.mics = mics;
            self.listed = Some(Instant::now());
            // process ids are reused: do not keep the names for ever
            self.names.clear();
            Ok(())
        }

        fn recording_on(&mut self, mic: &IMMDevice) -> Option<String> {
            // SAFETY: COM calls on valid interfaces
            unsafe {
                // a fresh session manager each time: one that is kept does
                // not list the sessions created after it
                let manager: IAudioSessionManager2 = mic.Activate(CLSCTX_ALL, None).ok()?;
                let list = manager.GetSessionEnumerator().ok()?;
                for i in 0..list.GetCount().ok()? {
                    let Ok(control) = list.GetSession(i) else {
                        continue;
                    };
                    if !control
                        .GetState()
                        .is_ok_and(|s| s == AudioSessionStateActive)
                    {
                        continue;
                    }
                    let Ok(pid) = control
                        .cast::<IAudioSessionControl2>()
                        .and_then(|c| c.GetProcessId())
                    else {
                        continue;
                    };
                    if pid == 0 || pid == self.own_pid {
                        continue;
                    }
                    let name = self.names.entry(pid).or_insert_with(|| process_name(pid));
                    if app_matches(name, &self.apps) {
                        return Some(name.clone());
                    }
                }
            }
            None
        }
    }

    impl DictationWatcher for SessionWatcher {
        fn recording(&mut self) -> Result<Option<String>> {
            if self.listed.is_none_or(|t| t.elapsed() >= RELIST) {
                self.relist()?;
            }
            let mics = std::mem::take(&mut self.mics);
            let found = mics.iter().find_map(|m| self.recording_on(m));
            self.mics = mics;
            Ok(found)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_list() {
        assert_eq!(parse_apps("Typeless"), ["typeless"]);
        assert_eq!(
            parse_apps(" Typeless.exe ; \"C:\\Apps\\Wispr Flow.exe\"，typeless,, "),
            ["typeless", "wispr flow"]
        );
        assert!(parse_apps(" , ;").is_empty());
    }

    #[test]
    fn app_names() {
        let apps = parse_apps(DEFAULT_APPS);
        assert!(app_matches("Typeless.exe", &apps));
        assert!(app_matches(
            "C:\\Program Files\\Typeless\\TYPELESS.EXE",
            &apps
        ));
        assert!(app_matches("Typeless Helper.exe", &apps));
        assert!(!app_matches("Discord.exe", &apps));
        assert!(!app_matches("", &apps));
        assert!(!app_matches("Typeless.exe", &[]));
    }

    #[test]
    fn mutes_at_once_and_holds() {
        let mut c = MuteControl::new();
        assert!(!c.step(POLL_S, false));
        assert!(c.step(POLL_S, true));
        // released: stays muted for HOLD_S, then opens and stays open
        let mut held = 0.0;
        while c.step(POLL_S, false) {
            held += POLL_S;
        }
        assert!((held - HOLD_S).abs() <= POLL_S, "{held}");
        assert!(!c.step(POLL_S, false));
        // recording again during the hold keeps it muted
        assert!(c.step(POLL_S, true));
        assert!(c.step(POLL_S, false));
        assert!(c.step(POLL_S, true));
        assert!(c.step(POLL_S, false));
    }

    #[test]
    fn gate_fades_without_a_step() {
        let mut g = MuteGate::new(48_000);
        let mut x = vec![1.0f32; 960];
        g.process(false, &mut x);
        assert!(x.iter().all(|&v| v == 1.0));
        g.process(true, &mut x);
        // 10 ms fade, monotonic, no jump larger than one step
        assert!(x[0] < 1.0 && x[0] > 0.99);
        assert!(x.windows(2).all(|w| w[1] <= w[0] && w[0] - w[1] < 0.003));
        assert_eq!(x[479], 0.0);
        assert!(x[480..].iter().all(|&v| v == 0.0));
        let mut x = vec![1.0f32; 960];
        g.process(true, &mut x);
        assert!(x.iter().all(|&v| v == 0.0));
        g.process(false, &mut x[..0]);
        let mut x = vec![1.0f32; 960];
        g.process(false, &mut x);
        assert!(x[0] > 0.0 && x[0] < 0.01);
        assert!(x.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(x[959], 1.0);
    }

    #[cfg(not(windows))]
    #[test]
    fn not_available_off_windows() {
        assert!(open_watcher("Typeless").is_err());
        assert!(open_watcher(" ").is_err());
    }
}
