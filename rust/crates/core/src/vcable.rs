//! Find the VB-Cable endpoints by role, and give them names that say what
//! they are for (`vcable.py`).
//!
//! VB-Cable's own names ("CABLE Input" is a speaker, "CABLE Output" is a mic)
//! are confusing in Discord / LINE device lists.  Windows lets an
//! administrator rename an endpoint (Sound control panel -> Properties);
//! that changes the endpoint's PKEY_Device_DeviceDesc, so the name everyone
//! sees becomes "<new name> (VB-Audio Virtual Cable)".  VB-Cable endpoints
//! this program does not use (CABLE In 16ch, CABLE-B ...) can also be
//! disabled, as the Sound control panel's "Disable" does, so they drop out
//! of every device list.
//!
//! Both need administrator rights, so the GUI re-runs this exe via UAC
//! ([`run_elevated`]) with comma-separated actions:
//!
//! ```text
//! desk-echo-guard --vcable rename,hide RESULT.json
//! desk-echo-guard --vcable restore     RESULT.json   (names back + re-enable)
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use echoguard_audio::{DeviceInfo, DeviceKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Role {
    pub key: &'static str,
    /// Speaker (render) or Mic (capture), as `DeviceInfo::kind`
    pub kind: DeviceKind,
    /// VB-Audio's endpoint description
    pub original: &'static str,
    /// what we rename it to
    pub renamed: &'static str,
}

pub const ROLES: [Role; 4] = [
    Role {
        key: "out",
        kind: DeviceKind::Speaker,
        original: "CABLE Input",
        renamed: "EchoGuard 內部輸出（勿選）",
    },
    Role {
        key: "virtual_mic",
        kind: DeviceKind::Mic,
        original: "CABLE Output",
        renamed: "回聲消除麥克風",
    },
    Role {
        key: "call_speaker",
        kind: DeviceKind::Speaker,
        original: "CABLE-A Input",
        renamed: "Siri防護喇叭",
    },
    Role {
        key: "call_in",
        kind: DeviceKind::Mic,
        original: "CABLE-A Output",
        renamed: "EchoGuard 內部輸入（勿選）",
    },
];

pub const DOWNLOAD_URL: &str = "https://vb-audio.com/Cable/";

/// Endpoint states (`DEVICE_STATE_ACTIVE`, `DEVICE_STATE_DISABLED`).
pub const STATE_ACTIVE: u32 = 1;
pub const STATE_DISABLED: u32 = 2;

/// The role with this key ("out", "virtual_mic", "call_speaker", "call_in").
pub fn role_by_key(key: &str) -> Option<&'static Role> {
    ROLES.iter().find(|r| r.key == key)
}

/// True if a friendly name "<desc> (<interface>)" (or bare "<desc>") has
/// exactly this description; "CABLE Input" must not match "CABLE In 16ch".
fn desc_is(name: &str, desc: &str) -> bool {
    let n = name.trim().to_lowercase();
    let d = desc.to_lowercase();
    n == d || n.starts_with(&format!("{d} ("))
}

/// The role a device name (original or renamed, with or without the
/// "(interface)" suffix) stands for.
pub fn role_of(name: &str, kind: Option<DeviceKind>) -> Option<&'static Role> {
    if name.is_empty() {
        return None;
    }
    ROLES.iter().find(|r| {
        (kind.is_none() || kind == Some(r.kind))
            && (desc_is(name, r.original) || desc_is(name, r.renamed))
    })
}

/// Name of the present device filling this role, either name style.
pub fn find(devices: &[DeviceInfo], role: &Role) -> Option<String> {
    devices
        .iter()
        .find(|d| d.kind == role.kind && role_of(&d.name, Some(role.kind)) == Some(role))
        .map(|d| d.name.clone())
}

/// Map a configured VB-Cable name to the device that is there now, so
/// "CABLE Input" still works after the rename (and the other way round).
/// Any other name is returned unchanged.
pub fn resolve(name: &str, kind: DeviceKind, devices: &[DeviceInfo]) -> String {
    match role_of(name, Some(kind)) {
        Some(r) => find(devices, r).unwrap_or_else(|| name.to_string()),
        None => name.to_string(),
    }
}

pub fn is_vbcable(name: &str) -> bool {
    name.to_lowercase().contains("vb-audio")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CableStatus {
    /// VB-Cable present (at least "out" and "virtual_mic")
    pub found: bool,
    /// every present role renamed
    pub renamed: bool,
    /// role key -> current device name
    pub names: BTreeMap<&'static str, Option<String>>,
    /// enabled VB-Cable devices this program has no use for
    pub unused: Vec<String>,
}

pub fn status(devices: &[DeviceInfo]) -> CableStatus {
    let names: BTreeMap<&'static str, Option<String>> =
        ROLES.iter().map(|r| (r.key, find(devices, r))).collect();
    let present: Vec<(&Role, &String)> = ROLES
        .iter()
        .filter_map(|r| names[r.key].as_ref().map(|n| (r, n)))
        .collect();
    let renamed = !present.is_empty() && present.iter().all(|(r, n)| desc_is(n, r.renamed));
    let unused = devices
        .iter()
        .filter(|d| is_vbcable(&d.name) && role_of(&d.name, Some(d.kind)).is_none())
        .map(|d| d.name.clone())
        .collect();
    CableStatus {
        found: names["out"].is_some() && names["virtual_mic"].is_some(),
        renamed,
        names,
        unused,
    }
}

/// Short name to tell the user to pick in Discord / LINE.
pub fn display_name(devices: &[DeviceInfo], key: &str) -> String {
    let Some(r) = role_by_key(key) else {
        return key.to_string();
    };
    match find(devices, r) {
        Some(name) if desc_is(&name, r.renamed) => r.renamed.to_string(),
        _ => r.original.to_string(),
    }
}

// ------------------------------------------------------------ endpoints

/// One render / capture endpoint as the Sound control panel sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub id: String,
    pub kind: DeviceKind,
    /// [`STATE_ACTIVE`], [`STATE_DISABLED`], ...
    pub state: u32,
    /// PKEY_Device_DeviceDesc, e.g. "CABLE Input"
    pub desc: String,
    /// PKEY_Device_FriendlyName, e.g. "CABLE Input (VB-Audio Virtual Cable)"
    pub name: String,
}

/// (endpoint, new description) for every VB-Cable endpoint to change.
pub fn plan(endpoints: &[Endpoint], restore: bool) -> Vec<(&Endpoint, &'static str)> {
    let mut todo = Vec::new();
    for ep in endpoints {
        if !is_vbcable(&ep.name) {
            continue; // never touch anything that is not VB-Cable
        }
        for r in &ROLES {
            let (src, dst) = if restore {
                (r.renamed, r.original)
            } else {
                (r.original, r.renamed)
            };
            if ep.kind == r.kind && ep.desc.trim().to_lowercase() == src.to_lowercase() {
                todo.push((ep, dst));
            }
        }
    }
    todo
}

/// VB-Cable endpoints without a role to disable (or, with `show`, to enable
/// again).  The ones this program uses are never disabled.
pub fn plan_visibility(endpoints: &[Endpoint], show: bool) -> Vec<&Endpoint> {
    let want = if show { STATE_DISABLED } else { STATE_ACTIVE };
    endpoints
        .iter()
        .filter(|ep| {
            is_vbcable(&ep.name) && ep.state == want && role_of(&ep.desc, Some(ep.kind)).is_none()
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CableAction {
    /// give the VB-Cable endpoints descriptive names
    Rename,
    /// disable the VB-Cable endpoints this program does not use
    Hide,
    /// names back + enable again
    Restore,
}

impl CableAction {
    pub const ALL: [CableAction; 3] =
        [CableAction::Rename, CableAction::Hide, CableAction::Restore];

    pub fn as_str(self) -> &'static str {
        match self {
            CableAction::Rename => "rename",
            CableAction::Hide => "hide",
            CableAction::Restore => "restore",
        }
    }
}

impl fmt::Display for CableAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CableAction {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        CableAction::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| anyhow!("unknown action {s:?}"))
    }
}

/// "rename,hide" -> [Rename, Hide]; empty or unknown entries are an error.
pub fn parse_actions(s: &str) -> Result<Vec<CableAction>> {
    s.split(',').map(str::parse).collect()
}

/// "rename,hide" from [Rename, Hide].
pub fn format_actions(actions: &[CableAction]) -> String {
    actions
        .iter()
        .map(|a| a.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Run rename / hide / restore (= names back + enable again) in order.
/// Needs administrator rights.  Returns what was changed.
pub fn apply(actions: &[CableAction]) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for &action in actions {
        let eps = list_endpoints()?;
        if matches!(action, CableAction::Rename | CableAction::Restore) {
            for (ep, new) in plan(&eps, action == CableAction::Restore) {
                set_description(&ep.id, new)?;
                done.push(format!("{} → {new}", ep.desc));
            }
        }
        if matches!(action, CableAction::Hide | CableAction::Restore) {
            let show = action == CableAction::Restore;
            for ep in plan_visibility(&eps, show) {
                set_enabled(&ep.id, show)?;
                done.push(format!(
                    "{} {}",
                    if show { "啟用" } else { "停用" },
                    ep.desc
                ));
            }
        }
    }
    Ok(done)
}

/// Interpret the result file the elevated helper wrote: (ok, message).
fn read_result(path: &Path) -> (bool, String) {
    let parsed = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let _ = std::fs::remove_file(path);
    let Some(r) = parsed else {
        return (false, "改名程式沒有回報結果".into());
    };
    if let Some(e) = r.get("error").filter(|e| !e.is_null()) {
        let msg = e.as_str().map_or_else(|| e.to_string(), str::to_string);
        if !msg.is_empty() {
            return (false, msg);
        }
    }
    let done: Vec<String> = r
        .get("done")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if done.is_empty() {
        (true, "沒有需要變更的裝置".into())
    } else {
        (true, done.join("、"))
    }
}

/// Elevated entry: `args` = the arguments after `--vcable`, i.e.
/// `["rename,hide", "RESULT.json"]`.  Writes `{"done": [...]}` or
/// `{"error": "..."}` to RESULT.json.  Exit code 0 ok, 1 failed, 2 usage.
pub fn elevated_main(args: &[String]) -> i32 {
    let actions = match args {
        [a, _] => parse_actions(a).ok(),
        _ => None,
    };
    let (Some(actions), [_, result]) = (actions, args) else {
        eprintln!("usage: desk-echo-guard --vcable rename[,hide]|hide|restore RESULT.json");
        return 2;
    };
    let r = match apply(&actions) {
        Ok(done) => serde_json::json!({ "done": done }),
        Err(e) => serde_json::json!({ "error": format!("{e:#}") }),
    };
    let failed = r.get("error").is_some();
    if let Err(e) = std::fs::write(result, r.to_string()) {
        eprintln!("cannot write {result}: {e}");
        return 1;
    }
    i32::from(failed)
}

/// Seconds to wait for the elevated helper.
pub const ELEVATED_TIMEOUT_S: u32 = 120;

/// Run `actions` in an elevated copy of this exe (UAC prompt) and wait for
/// it.  Returns (ok, message for the user).
pub fn run_elevated(actions: &[CableAction]) -> (bool, String) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let result = std::env::temp_dir().join(format!(
        "echoguard-vcable-{}-{nanos}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&result);
    let params = format!(
        "--vcable {} \"{}\"",
        format_actions(actions),
        result.display()
    );
    match imp::run_elevated_and_wait(&params, ELEVATED_TIMEOUT_S * 1000) {
        Ok(true) => read_result(&result),
        Ok(false) => (false, "已取消（需要在 Windows 的確認視窗按「是」）".into()),
        Err(e) => (false, format!("{e:#}")),
    }
}

/// Active and disabled render + capture endpoints.
pub fn list_endpoints() -> Result<Vec<Endpoint>> {
    imp::list_endpoints()
}

/// Rename one endpoint (needs administrator rights).
pub fn set_description(dev_id: &str, desc: &str) -> Result<()> {
    imp::set_description(dev_id, desc)
}

/// Enable / disable one endpoint (needs administrator rights).
pub fn set_enabled(dev_id: &str, enabled: bool) -> Result<()> {
    imp::set_enabled(dev_id, enabled)
}

/// Endpoint kind from its id: "{0.0.0.00000000}.{guid}" is render,
/// "{0.0.1.00000000}.{guid}" is capture.
pub fn kind_from_endpoint_id(id: &str) -> DeviceKind {
    if id.starts_with("{0.0.1.") {
        DeviceKind::Mic
    } else {
        DeviceKind::Speaker
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::bail;

    use super::*;

    const MSG: &str = "VB-Cable management is only supported on Windows";

    pub fn list_endpoints() -> Result<Vec<Endpoint>> {
        bail!(MSG)
    }
    pub fn set_description(_dev_id: &str, _desc: &str) -> Result<()> {
        bail!(MSG)
    }
    pub fn set_enabled(_dev_id: &str, _enabled: bool) -> Result<()> {
        bail!(MSG)
    }
    pub fn run_elevated_and_wait(_params: &str, _timeout_ms: u32) -> Result<bool> {
        bail!(MSG)
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use anyhow::Context;
    use windows::core::{IUnknown, Interface, GUID, HRESULT, HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Devices::FunctionDiscovery::{
        PKEY_Device_DeviceDesc, PKEY_Device_FriendlyName,
    };
    use windows::Win32::Foundation::{CloseHandle, PROPERTYKEY};
    use windows::Win32::Media::Audio::{
        eAll, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE,
    };
    use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ, STGM_READWRITE,
    };
    use windows::Win32::System::Threading::WaitForSingleObject;
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    use super::*;

    /// Initialises COM on this thread for the guard's lifetime.  If the
    /// thread already uses another apartment model (e.g. a GUI thread),
    /// CoInitializeEx fails with RPC_E_CHANGED_MODE and COM is usable as is.
    struct Com(bool);

    impl Com {
        fn init() -> Com {
            // SAFETY: plain COM initialisation; balanced in Drop on success
            Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: matches the successful CoInitializeEx above
                unsafe { CoUninitialize() };
            }
        }
    }

    fn enumerator() -> Result<IMMDeviceEnumerator> {
        // SAFETY: standard COM activation of a documented class
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .context("CoCreateInstance(MMDeviceEnumerator) failed")
    }

    /// A string property, or "" if missing / not a string.
    fn string_prop(store: &IPropertyStore, key: &PROPERTYKEY) -> String {
        // SAFETY: GetValue fills a PROPVARIANT we own and clear afterwards;
        // the union is only read as pwszVal when vt says it is a LPWSTR
        unsafe {
            let Ok(mut pv) = store.GetValue(key) else {
                return String::new();
            };
            let inner = &pv.Anonymous.Anonymous;
            let s = if inner.vt == VT_LPWSTR && !inner.Anonymous.pwszVal.is_null() {
                inner.Anonymous.pwszVal.to_string().unwrap_or_default()
            } else {
                String::new()
            };
            let _ = PropVariantClear(&mut pv);
            s
        }
    }

    pub fn list_endpoints() -> Result<Vec<Endpoint>> {
        let _com = Com::init();
        let enumerator = enumerator()?;
        let mut out = Vec::new();
        // SAFETY: COM calls on valid interface pointers; the id string is
        // freed with CoTaskMemFree as documented for IMMDevice::GetId
        unsafe {
            let coll = enumerator
                .EnumAudioEndpoints(eAll, DEVICE_STATE(STATE_ACTIVE | STATE_DISABLED))
                .context("EnumAudioEndpoints failed")?;
            for i in 0..coll.GetCount()? {
                let dev = coll.Item(i)?;
                let pid: PWSTR = dev.GetId()?;
                let id = pid.to_string().unwrap_or_default();
                CoTaskMemFree(Some(pid.0 as *const c_void));
                let state = dev.GetState()?.0;
                let store = dev.OpenPropertyStore(STGM_READ)?;
                out.push(Endpoint {
                    kind: kind_from_endpoint_id(&id),
                    id,
                    state,
                    desc: string_prop(&store, &PKEY_Device_DeviceDesc),
                    name: string_prop(&store, &PKEY_Device_FriendlyName),
                });
            }
        }
        Ok(out)
    }

    pub fn set_description(dev_id: &str, desc: &str) -> Result<()> {
        let _com = Com::init();
        let enumerator = enumerator()?;
        let id = HSTRING::from(dev_id);
        // NUL-terminated UTF-16 buffer that outlives SetValue; the
        // PROPVARIANT only borrows it, so it must NOT be PropVariantClear'ed
        let mut text: Vec<u16> = desc.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: COM calls on valid interface pointers; `pv` points into
        // `text`, which lives until the end of this block
        unsafe {
            let dev = enumerator
                .GetDevice(&id)
                .with_context(|| format!("GetDevice({dev_id}) failed"))?;
            let store = dev
                .OpenPropertyStore(STGM_READWRITE)
                .context("OpenPropertyStore(STGM_READWRITE) failed")?;
            let mut pv = PROPVARIANT::default();
            {
                let inner = &mut *pv.Anonymous.Anonymous;
                inner.vt = VT_LPWSTR;
                inner.Anonymous.pwszVal = PWSTR(text.as_mut_ptr());
            }
            store
                .SetValue(&PKEY_Device_DeviceDesc, &pv)
                .context("IPropertyStore::SetValue failed")?;
            store.Commit().context("IPropertyStore::Commit failed")?;
        }
        Ok(())
    }

    // IPolicyConfig is undocumented but stable since Windows 7; it is what
    // the Sound control panel uses to disable / enable an endpoint.
    const CLSID_POLICY_CONFIG_CLIENT: GUID =
        GUID::from_u128(0x870AF99C_171D_4F9E_AF0D_E63DF40C2BC9);
    const IID_IPOLICY_CONFIG: GUID = GUID::from_u128(0xF8679F50_850A_41CF_9C72_430F290290C8);

    /// Raw vtable: IUnknown (slots 0-2), 11 methods we never call (3-13),
    /// then SetEndpointVisibility (slot 14).
    #[repr(C)]
    struct PolicyConfigVtbl {
        _unused: [*const c_void; 14],
        set_endpoint_visibility:
            unsafe extern "system" fn(this: *mut c_void, id: PCWSTR, visible: i32) -> HRESULT,
    }

    pub fn set_enabled(dev_id: &str, enabled: bool) -> Result<()> {
        let _com = Com::init();
        // SAFETY: we activate PolicyConfigClient, QueryInterface for the
        // IPolicyConfig IID and call slot 14 with (this, id, BOOL).  The
        // returned pointer is wrapped in IUnknown so it is Released on drop.
        unsafe {
            let unk: IUnknown = CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)
                .context("CoCreateInstance(PolicyConfig) failed")?;
            let mut raw: *mut c_void = std::ptr::null_mut();
            unk.query(&IID_IPOLICY_CONFIG, &mut raw)
                .ok()
                .context("QueryInterface(IPolicyConfig) failed")?;
            let pc = IUnknown::from_raw(raw);
            let vtbl = *(pc.as_raw() as *const *const PolicyConfigVtbl);
            let id = HSTRING::from(dev_id);
            ((*vtbl).set_endpoint_visibility)(pc.as_raw(), PCWSTR(id.as_ptr()), i32::from(enabled))
                .ok()
                .context("IPolicyConfig::SetEndpointVisibility failed")?;
        }
        Ok(())
    }

    /// ShellExecuteExW("runas", current exe, params) and wait up to
    /// `timeout_ms`.  Ok(false) if the user declined the UAC prompt.
    pub fn run_elevated_and_wait(params: &str, timeout_ms: u32) -> Result<bool> {
        let exe = std::env::current_exe().context("cannot locate this program")?;
        let file = HSTRING::from(exe.as_os_str());
        let params = HSTRING::from(params);
        let verb = HSTRING::from("runas");
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: `sei` and the strings it points to outlive the call; the
        // process handle is closed after waiting
        unsafe {
            if ShellExecuteExW(&mut sei).is_err() {
                return Ok(false); // declined (ERROR_CANCELLED) or failed to start
            }
            if !sei.hProcess.is_invalid() {
                WaitForSingleObject(sei.hProcess, timeout_ms);
                let _ = CloseHandle(sei.hProcess);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(kind: DeviceKind, name: &str, id: &str, channels: u16) -> DeviceInfo {
        DeviceInfo {
            kind,
            name: name.into(),
            id: id.into(),
            channels,
            default: false,
        }
    }

    fn original() -> Vec<DeviceInfo> {
        use DeviceKind::*;
        vec![
            dev(Speaker, "CABLE Input (VB-Audio Virtual Cable)", "1", 2),
            dev(Speaker, "CABLE In 16ch (VB-Audio Virtual Cable)", "2", 16),
            dev(Speaker, "CABLE-A Input (VB-Audio Cable A)", "3", 2),
            dev(Mic, "CABLE Output (VB-Audio Virtual Cable)", "4", 2),
            dev(Mic, "CABLE-A Output (VB-Audio Cable A)", "5", 2),
            dev(Speaker, "Speakers (Realtek(R) Audio)", "6", 2),
        ]
    }

    fn renamed() -> Vec<DeviceInfo> {
        original()
            .into_iter()
            .map(|d| match role_of(&d.name, Some(d.kind)) {
                Some(r) => DeviceInfo {
                    name: d.name.replacen(r.original, r.renamed, 1),
                    ..d
                },
                None => d,
            })
            .collect()
    }

    #[test]
    fn role_matching_is_exact_on_description() {
        let key = |n: &str| role_of(n, None).map(|r| r.key);
        assert_eq!(key("CABLE Input (VB-Audio Virtual Cable)"), Some("out"));
        assert_eq!(key("CABLE Input"), Some("out"));
        assert_eq!(key("CABLE In 16ch (VB-Audio Virtual Cable)"), None);
        assert_eq!(
            key("CABLE-A Input (VB-Audio Cable A)"),
            Some("call_speaker")
        );
        assert_eq!(
            key("回聲消除麥克風 (VB-Audio Virtual Cable)"),
            Some("virtual_mic")
        );
        assert_eq!(role_of("CABLE Input", Some(DeviceKind::Mic)), None);
        assert_eq!(role_of("", None), None);
    }

    #[test]
    fn resolve_follows_rename_both_ways() {
        use DeviceKind::*;
        let (orig, ren) = (original(), renamed());
        assert_eq!(
            resolve("CABLE Input", Speaker, &ren),
            "EchoGuard 內部輸出（勿選） (VB-Audio Virtual Cable)"
        );
        assert_eq!(
            resolve("CABLE-A Output (VB-Audio Cable A)", Mic, &ren),
            "EchoGuard 內部輸入（勿選） (VB-Audio Cable A)"
        );
        assert_eq!(
            resolve("EchoGuard 內部輸出（勿選）", Speaker, &orig),
            "CABLE Input (VB-Audio Virtual Cable)"
        );
        assert_eq!(resolve("Speakers", Speaker, &ren), "Speakers");
        assert_eq!(resolve("CABLE Input", Speaker, &[]), "CABLE Input");
    }

    #[test]
    fn status_and_display_names() {
        let (orig, ren) = (original(), renamed());
        let s = status(&orig);
        assert!(s.found && !s.renamed);
        assert_eq!(display_name(&orig, "virtual_mic"), "CABLE Output");
        let s = status(&ren);
        assert!(s.found && s.renamed);
        assert_eq!(display_name(&ren, "virtual_mic"), "回聲消除麥克風");
        assert_eq!(display_name(&ren, "call_speaker"), "Siri防護喇叭");
        assert!(!status(&orig[orig.len() - 1..]).found);
        assert_eq!(s.names.len(), 4);
        assert_eq!(
            s.names["virtual_mic"].as_deref(),
            Some("回聲消除麥克風 (VB-Audio Virtual Cable)")
        );
    }

    fn ep(id: &str, kind: DeviceKind, desc: &str, name: &str, state: u32) -> Endpoint {
        Endpoint {
            id: id.into(),
            kind,
            state,
            desc: desc.into(),
            name: name.into(),
        }
    }

    #[test]
    fn plan_only_touches_vb_audio_endpoints() {
        use DeviceKind::*;
        let eps = vec![
            ep(
                "a",
                Speaker,
                "CABLE Input",
                "CABLE Input (VB-Audio Virtual Cable)",
                1,
            ),
            ep(
                "b",
                Speaker,
                "CABLE In 16ch",
                "CABLE In 16ch (VB-Audio Virtual Cable)",
                1,
            ),
            ep(
                "c",
                Mic,
                "CABLE Output",
                "CABLE Output (VB-Audio Virtual Cable)",
                1,
            ),
            ep(
                "d",
                Speaker,
                "CABLE Input",
                "CABLE Input (Some Other Driver)",
                1,
            ),
        ];
        let todo: BTreeMap<String, &str> = plan(&eps, false)
            .into_iter()
            .map(|(e, n)| (e.id.clone(), n))
            .collect();
        let want: BTreeMap<String, &str> = [
            ("a".to_string(), "EchoGuard 內部輸出（勿選）"),
            ("c".to_string(), "回聲消除麥克風"),
        ]
        .into();
        assert_eq!(todo, want);
        let ren: Vec<Endpoint> = eps
            .iter()
            .map(|e| Endpoint {
                desc: todo.get(&e.id).map_or(e.desc.clone(), |n| n.to_string()),
                ..e.clone()
            })
            .collect();
        assert!(plan(&ren, false).is_empty());
        let back: BTreeMap<String, &str> = plan(&ren, true)
            .into_iter()
            .map(|(e, n)| (e.id.clone(), n))
            .collect();
        let want: BTreeMap<String, &str> = [
            ("a".to_string(), "CABLE Input"),
            ("c".to_string(), "CABLE Output"),
        ]
        .into();
        assert_eq!(back, want);
    }

    #[test]
    fn status_lists_unused_vbcable_devices() {
        let ren = renamed();
        let unused = vec!["CABLE In 16ch (VB-Audio Virtual Cable)".to_string()];
        assert_eq!(status(&original()).unused, unused);
        assert_eq!(status(&ren).unused, unused);
        let without: Vec<DeviceInfo> = ren[..1].iter().chain(&ren[2..]).cloned().collect();
        assert!(status(&without).unused.is_empty());
    }

    #[test]
    fn plan_visibility_never_disables_used_endpoints() {
        use DeviceKind::*;
        let e = |i: &str, kind, desc: &str, iface: &str, state| {
            ep(i, kind, desc, &format!("{desc} ({iface})"), state)
        };
        let eps = vec![
            e(
                "a",
                Speaker,
                "CABLE Input",
                "VB-Audio Virtual Cable",
                STATE_ACTIVE,
            ),
            e(
                "b",
                Speaker,
                "CABLE In 16ch",
                "VB-Audio Virtual Cable",
                STATE_ACTIVE,
            ),
            e(
                "c",
                Mic,
                "回聲消除麥克風",
                "VB-Audio Virtual Cable",
                STATE_ACTIVE,
            ),
            e(
                "d",
                Speaker,
                "CABLE-B Input",
                "VB-Audio Cable B",
                STATE_ACTIVE,
            ),
            e(
                "e",
                Mic,
                "CABLE-B Output",
                "VB-Audio Cable B",
                STATE_DISABLED,
            ),
            e("f", Speaker, "Speakers", "Realtek(R) Audio", STATE_ACTIVE),
        ];
        let ids = |v: Vec<&Endpoint>| v.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(plan_visibility(&eps, false)), ["b", "d"]);
        assert_eq!(ids(plan_visibility(&eps, true)), ["e"]);
    }

    #[test]
    fn elevated_entry_rejects_unknown_actions() {
        let r = std::env::temp_dir().join("echoguard-core-test-r.json");
        let r = r.to_string_lossy().to_string();
        assert_eq!(elevated_main(&["rename,format-disk".into(), r.clone()]), 2);
        assert_eq!(elevated_main(&["rename".into()]), 2);
        assert_eq!(elevated_main(&["".into(), r]), 2);
    }

    #[test]
    fn actions_parse_and_format() {
        let a = parse_actions("rename,hide").unwrap();
        assert_eq!(a, [CableAction::Rename, CableAction::Hide]);
        assert_eq!(format_actions(&a), "rename,hide");
        assert!(parse_actions("restore,").is_err());
    }

    #[test]
    fn result_file_messages() {
        let p = std::env::temp_dir().join(format!(
            "echoguard-core-test-{}-res.json",
            std::process::id()
        ));
        assert_eq!(read_result(&p), (false, "改名程式沒有回報結果".to_string()));
        std::fs::write(&p, r#"{"done": ["CABLE Input → 回聲", "停用 X"]}"#).unwrap();
        assert_eq!(
            read_result(&p),
            (true, "CABLE Input → 回聲、停用 X".to_string())
        );
        assert!(!p.exists());
        std::fs::write(&p, r#"{"done": []}"#).unwrap();
        assert_eq!(read_result(&p), (true, "沒有需要變更的裝置".to_string()));
        std::fs::write(&p, r#"{"error": "OSError: boom"}"#).unwrap();
        assert_eq!(read_result(&p), (false, "OSError: boom".to_string()));
    }

    #[test]
    fn endpoint_kind_from_id() {
        assert_eq!(
            kind_from_endpoint_id("{0.0.1.00000000}.{abc}"),
            DeviceKind::Mic
        );
        assert_eq!(
            kind_from_endpoint_id("{0.0.0.00000000}.{abc}"),
            DeviceKind::Speaker
        );
    }
}
