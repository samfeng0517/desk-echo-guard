//! "Ducking": while the call partner speaks, turn the other programs down.
//!
//! Only works when the call audio runs through this program (the Siri guard
//! path): the guard thread notes when the incoming call audio is loud
//! enough to be speech, and a control thread lowers the session volume of
//! every other program playing on the speakers (the Windows volume mixer),
//! then brings it back when the partner has been quiet for a moment.
//!
//! [`DuckControl`] is the platform independent part (activity -> smoothed
//! gain); [`open_ducker`] gives the Windows volume-mixer side.

use anyhow::Result;

/// Call audio above this RMS level counts as the partner speaking.
pub const SPEECH_LEVEL_DB: f64 = -45.0;
/// Keep the others down this long after the last speech (bridges the pauses
/// between words).
pub const HOLD_S: f64 = 0.8;
/// Time constants of the volume change.
const ATTACK_S: f64 = 0.05;
const RELEASE_S: f64 = 0.35;
/// Volume steps smaller than this are not sent to the mixer.
const MIN_STEP: f32 = 0.01;

/// True when a block of call audio is loud enough to be speech.
pub fn is_speech(block: &[f32]) -> bool {
    if block.is_empty() {
        return false;
    }
    let e: f64 = block.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / block.len() as f64;
    e > 10f64.powf(SPEECH_LEVEL_DB / 10.0)
}

/// Gain of the other programs for a setting of "lower by `percent` %".
pub fn ducked_gain(percent: f64) -> f32 {
    (1.0 - percent.clamp(0.0, 100.0) / 100.0) as f32
}

/// Speech activity -> smoothed gain for the other programs.
pub struct DuckControl {
    floor: f32,
    gain: f32,
    sent: f32,
}

impl DuckControl {
    /// `percent`: how much the others are lowered (0 = not at all, 100 = muted).
    pub fn new(percent: f64) -> Self {
        Self {
            floor: ducked_gain(percent),
            gain: 1.0,
            sent: 1.0,
        }
    }

    /// Advance by `dt` seconds; `quiet_s` is the time since the partner was
    /// last heard.  Returns the gain to apply when it changed enough (always
    /// when the end point, fully ducked or fully restored, is reached).
    pub fn step(&mut self, dt: f64, quiet_s: f64) -> Option<f32> {
        let target = if quiet_s < HOLD_S { self.floor } else { 1.0 };
        let tau = if target < self.gain {
            ATTACK_S
        } else {
            RELEASE_S
        };
        let k = (1.0 - (-dt / tau).exp()) as f32;
        self.gain += (target - self.gain) * k;
        if (self.gain - target).abs() < MIN_STEP / 2.0 {
            self.gain = target;
        }
        let arrived = self.gain == target && self.sent != target;
        if arrived || (self.gain - self.sent).abs() >= MIN_STEP {
            self.sent = self.gain;
            Some(self.gain)
        } else {
            None
        }
    }

    /// The gain last returned by [`step`](Self::step).
    pub fn applied(&self) -> f32 {
        self.sent
    }
}

/// The volume-mixer side: scales the volume of every other program on the
/// speakers.  `set_gain(1.0)` (also done on drop) restores the volumes.
pub trait SessionDucker: Send {
    fn set_gain(&mut self, gain: f32) -> Result<()>;
}

/// Ducker for the speaker endpoint `speaker` (a name as in the settings;
/// `None` = the Windows default).  Must be called, used and dropped on one
/// thread.  Errors on other platforms.
pub fn open_ducker(speaker: Option<&str>) -> Result<Box<dyn SessionDucker>> {
    imp::open(speaker)
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn open(_speaker: Option<&str>) -> Result<Box<dyn SessionDucker>> {
        anyhow::bail!("lowering other programs' volume needs the Windows volume mixer")
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use anyhow::Context;
    use windows::core::{Interface, HSTRING, PWSTR};
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
        IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    use crate::vcable;
    use echoguard_audio::{match_device, DeviceInfo, DeviceKind};

    /// One program's session while it is turned down.
    struct Ducked {
        volume: ISimpleAudioVolume,
        /// persistent session id (same after the program restarts)
        key: String,
        original: f32,
        /// what we last set, to notice the user moving the slider meanwhile
        set: f32,
    }

    /// A session manager and its sessions: (instance id, persistent id, volume).
    type Listed = (IAudioSessionManager2, Vec<(String, String, ISimpleAudioVolume)>);

    /// While the others are turned down, programs that start playing are
    /// looked for this often (not on every volume step, see `sessions`).
    const RELOOK: Duration = Duration::from_secs(1);

    pub struct MixerDucker {
        /// the speaker endpoint
        device: IMMDevice,
        own_pid: u32,
        /// by session *instance* id
        ducked: HashMap<String, Ducked>,
        /// the session managers the volume controls in `ducked` came from;
        /// released together with them when the volumes are restored
        managers: Vec<IAudioSessionManager2>,
        /// when the sessions were last listed while turned down
        looked: Option<Instant>,
        gain: f32,
    }

    // SAFETY: the COM objects live in the multithreaded apartment and are
    // free-threaded; the ducker is still created, used and dropped by the
    // one control thread.
    unsafe impl Send for MixerDucker {}

    /// Volumes to put back if the program died while others were turned
    /// down: `persistent session id \t original volume` per line.
    fn restore_file() -> PathBuf {
        std::env::temp_dir().join("DeskEchoGuard-ducked-volumes.txt")
    }

    /// Take ownership of a CoTaskMem string.
    unsafe fn take_string(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.as_ptr() as *const _));
        s
    }

    fn speaker_device(
        enumerator: &IMMDeviceEnumerator,
        speaker: Option<&str>,
    ) -> Result<IMMDevice> {
        // SAFETY: COM calls on a valid enumerator
        unsafe {
            let Some(name) = speaker.filter(|n| !n.trim().is_empty()) else {
                return enumerator
                    .GetDefaultAudioEndpoint(eRender, eConsole)
                    .context("no default speaker");
            };
            let items: Vec<DeviceInfo> = vcable::list_endpoints()?
                .into_iter()
                .filter(|e| e.kind == DeviceKind::Speaker && e.state == vcable::STATE_ACTIVE)
                .map(|e| DeviceInfo {
                    kind: DeviceKind::Speaker,
                    name: e.name,
                    id: e.id,
                    channels: 2,
                    default: false,
                })
                .collect();
            let dev = match_device(&items, name, "speaker")?;
            enumerator
                .GetDevice(&HSTRING::from(dev.id.as_str()))
                .context("speaker endpoint not found")
        }
    }

    pub fn open(speaker: Option<&str>) -> Result<Box<dyn SessionDucker>> {
        // SAFETY: plain COM initialisation / activation; every interface is
        // owned by the returned value
        unsafe {
            // never balanced with CoUninitialize: the control thread ends
            // right after dropping the ducker, and its interfaces must not
            // outlive the apartment
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("CoCreateInstance(MMDeviceEnumerator) failed")?;
            let device = speaker_device(&enumerator, speaker)?;
            let mut d = MixerDucker {
                device,
                own_pid: std::process::id(),
                ducked: HashMap::new(),
                managers: Vec::new(),
                looked: None,
                gain: 1.0,
            };
            // fails here if the speaker has no volume mixer
            d.sessions()?;
            d.restore_after_crash();
            Ok(Box::new(d))
        }
    }

    impl MixerDucker {
        /// Every other program's session on the speaker:
        /// (instance id, persistent id, volume control), and the session
        /// manager they belong to.
        ///
        /// Each call uses a session manager of its own, and a manager is
        /// never listed twice: do not reuse one from `self.managers`.
        /// Listing the sessions of a manager again leaves notification
        /// registrations behind in this process (also after the listed
        /// objects, and later the manager, are released), and every
        /// `SetMasterVolume` from then on queues data for each of them in
        /// the kernel (paged pool, tags EtwD / Etwr) that is only freed when
        /// the process ends.  0.4.6 / 0.4.7 listed on one manager on every
        /// volume step and so used up tens of MB of kernel memory per second
        /// while the call partner spoke.  Measured on Windows 11 (26100 /
        /// 26300): one listing per manager leaves nothing behind.  A manager
        /// is kept only while volume controls from it are in use.
        fn sessions(&self) -> Result<Listed> {
            let mut out = Vec::new();
            // SAFETY: COM calls on valid interfaces; returned strings are
            // CoTaskMem allocations we free in `take_string`
            let manager = unsafe {
                let manager: IAudioSessionManager2 = self
                    .device
                    .Activate(CLSCTX_ALL, None)
                    .context("the speaker has no volume mixer")?;
                let list = manager.GetSessionEnumerator()?;
                for i in 0..list.GetCount()? {
                    let Ok(control) = list.GetSession(i) else {
                        continue;
                    };
                    let Ok(c2) = control.cast::<IAudioSessionControl2>() else {
                        continue;
                    };
                    // our own playback (the partner's voice) stays as it is
                    if c2.GetProcessId().is_ok_and(|pid| pid == self.own_pid) {
                        continue;
                    }
                    let (Ok(instance), Ok(key)) =
                        (c2.GetSessionInstanceIdentifier(), c2.GetSessionIdentifier())
                    else {
                        continue;
                    };
                    let (instance, key) = (take_string(instance), take_string(key));
                    if let Ok(volume) = control.cast::<ISimpleAudioVolume>() {
                        out.push((instance, key, volume));
                    }
                }
                manager
            };
            Ok((manager, out))
        }

        fn write_restore_file(&self) {
            let text: String = self
                .ducked
                .values()
                .map(|d| format!("{}\t{}\n", d.key, d.original))
                .collect();
            let _ = std::fs::write(restore_file(), text);
        }

        /// A previous run died while programs were turned down: put the
        /// volumes it noted back.
        fn restore_after_crash(&mut self) {
            let Ok(text) = std::fs::read_to_string(restore_file()) else {
                return;
            };
            let saved: HashMap<&str, f32> = text
                .lines()
                .filter_map(|l| l.rsplit_once('\t'))
                .filter_map(|(k, v)| Some((k, v.parse().ok()?)))
                .collect();
            if let Ok((_manager, sessions)) = self.sessions() {
                for (_, key, volume) in sessions {
                    if let Some(&v) = saved.get(key.as_str()) {
                        // SAFETY: valid interface, null event context
                        let _ = unsafe { volume.SetMasterVolume(v, std::ptr::null()) };
                    }
                }
            }
            let _ = std::fs::remove_file(restore_file());
        }
    }

    impl SessionDucker for MixerDucker {
        fn set_gain(&mut self, gain: f32) -> Result<()> {
            let gain = gain.clamp(0.0, 1.0);
            let restoring = gain >= 1.0;
            let mut changed = false;
            // list the sessions when the volumes start to go down, and then
            // now and again for programs that started playing meanwhile;
            // not on every step (see `sessions`)
            let look = !restoring && self.looked.is_none_or(|t| t.elapsed() >= RELOOK);
            // SAFETY: COM calls on valid interfaces, null event context
            unsafe {
                if look {
                    let (manager, sessions) = self.sessions()?;
                    self.looked = Some(Instant::now());
                    for (instance, key, volume) in sessions {
                        if let std::collections::hash_map::Entry::Vacant(slot) =
                            self.ducked.entry(instance)
                        {
                            let original = volume.GetMasterVolume().unwrap_or(1.0);
                            slot.insert(Ducked {
                                volume,
                                key,
                                original,
                                set: original,
                            });
                            changed = true;
                        }
                    }
                    if changed {
                        // volume controls of this manager are in use now
                        self.managers.push(manager);
                    }
                }
                for d in self.ducked.values_mut() {
                    // the user moved the slider while it was down: keep the
                    // new setting (scaled back up)
                    if let Ok(now) = d.volume.GetMasterVolume() {
                        if (now - d.set).abs() > 0.005 && self.gain > 0.01 {
                            d.original = (now / self.gain).clamp(0.0, 1.0);
                        }
                    }
                    d.set = d.original * gain;
                    // a session that ended meanwhile just fails here
                    let _ = d.volume.SetMasterVolume(d.set, std::ptr::null());
                }
            }
            self.gain = gain;
            if restoring {
                // the volume controls first, then the managers they are from
                self.ducked.clear();
                self.managers.clear();
                self.looked = None;
                let _ = std::fs::remove_file(restore_file());
            } else if changed {
                self.write_restore_file();
            }
            Ok(())
        }
    }

    impl Drop for MixerDucker {
        fn drop(&mut self) {
            let _ = self.set_gain(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_level() {
        assert!(!is_speech(&[]));
        assert!(!is_speech(&[0.0; 480]));
        assert!(!is_speech(&[0.001; 480])); // -60 dB
        assert!(is_speech(&[0.02; 480])); // -34 dB
    }

    #[test]
    fn ducks_fast_holds_and_recovers() {
        let mut c = DuckControl::new(70.0);
        let dt = 0.02;
        let mut sent = Vec::new();
        // partner speaks for 1 s
        for _ in 0..50 {
            if let Some(g) = c.step(dt, 0.0) {
                sent.push(g);
            }
        }
        assert!((c.applied() - 0.3).abs() < 1e-6, "{}", c.applied());
        // down within 0.3 s, in several steps, monotonic
        assert!(sent.len() > 3 && sent.len() < 20, "{sent:?}");
        assert!(sent.windows(2).all(|w| w[1] < w[0]));
        // short pause: stays down, nothing sent
        for k in 0..30 {
            assert_eq!(c.step(dt, k as f64 * dt), None);
        }
        // long silence: back to exactly 1
        let mut up = Vec::new();
        for k in 0..150 {
            if let Some(g) = c.step(dt, HOLD_S + k as f64 * dt) {
                up.push(g);
            }
        }
        assert_eq!(up.last(), Some(&1.0));
        assert!(up.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(c.step(dt, 10.0), None);
    }

    #[test]
    fn percent_range() {
        assert_eq!(ducked_gain(0.0), 1.0);
        assert_eq!(ducked_gain(100.0), 0.0);
        assert_eq!(ducked_gain(250.0), 0.0);
        assert_eq!(ducked_gain(-5.0), 1.0);
        // 0 % never sends anything
        let mut c = DuckControl::new(0.0);
        assert!((0..100).all(|_| c.step(0.02, 0.0).is_none()));
    }

    #[cfg(not(windows))]
    #[test]
    fn not_available_off_windows() {
        assert!(open_ducker(None).is_err());
    }
}
