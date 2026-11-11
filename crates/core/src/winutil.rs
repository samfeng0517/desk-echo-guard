//! Windows helpers for the GUI (`winutil.py`): "start with Windows" (a Task
//! Scheduler logon task), single instance, and opening the Sound settings /
//! a folder.  Elsewhere they do nothing or return an error.
//!
//! Sign-in start is a logon task rather than the HKCU Run key: Explorer
//! throttles Run entries (minutes on a machine with many startup apps),
//! while a logon task fires as soon as the user signs in and carries its own
//! priority.  A Run entry left by v0.4.4 or older is moved over to the task.

use std::path::Path;

use anyhow::Result;

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_NAME: &str = "DeskEchoGuard";
pub const TASK_NAME: &str = "DeskEchoGuard";
/// Task Scheduler priority 0-10.  The default 7 is below-normal CPU *and*
/// low I/O priority; 3 is above normal so sign-in start isn't starved.
pub const TASK_PRIORITY: u32 = 3;
pub const AUTOSTART_FLAG: &str = "--autostart";
pub const MUTEX_NAME: &str = r"Local\DeskEchoGuard.SingleInstance";

/// Command line for this exe: open the GUI and start right away.
pub fn autostart_command_for(exe: &Path) -> String {
    format!("\"{}\" {AUTOSTART_FLAG}", exe.display())
}

/// Command line Windows runs at sign-in (this exe + `--autostart`).
pub fn autostart_command() -> Result<String> {
    Ok(autostart_command_for(&std::env::current_exe()?))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Task Scheduler definition: run `exe --autostart` when `user` signs in.
pub fn task_xml(exe: &Path, user: &str) -> String {
    let user = xml_escape(user);
    let exe = xml_escape(&exe.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Desk Echo Guard: start at sign-in</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId>
    <LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>{TASK_PRIORITY}</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{AUTOSTART_FLAG}</Arguments></Exec></Actions>
</Task>
"#
    )
}

fn xml_text<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let len = xml[start..].find(&format!("</{tag}>"))?;
    Some(xml[start..start + len].trim())
}

/// Command line of a registered task, or "" when it is not one this version
/// wrote (another priority, no exec action), so refresh_autostart rewrites it.
pub fn task_command(xml: &str) -> String {
    let prio = xml_text(xml, "Priority").unwrap_or("7");
    let (Some(exe), true) = (xml_text(xml, "Command"), prio == TASK_PRIORITY.to_string()) else {
        return String::new();
    };
    let args = xml_text(xml, "Arguments").unwrap_or("");
    format!("\"{}\" {}", xml_unescape(exe), xml_unescape(args))
}

/// The sign-in command (logon task, else an old Run entry), None if off.
pub fn get_autostart() -> Option<String> {
    imp::get_task().or_else(imp::get_run_key)
}

/// Turn "start with Windows" on (pointing at this exe) or off.
pub fn set_autostart(enabled: bool) -> Result<()> {
    if enabled {
        imp::register_task(&task_xml(&std::env::current_exe()?, &current_user()))?;
    } else {
        imp::delete_task()?;
    }
    imp::delete_run_key() // the task replaces it; never start twice
}

fn current_user() -> String {
    let var = |k| std::env::var(k).unwrap_or_default();
    format!("{}\\{}", var("USERDOMAIN"), var("USERNAME"))
}

/// Whether autostart must be rewritten: an old Run entry is left over, or
/// the task points elsewhere / is an older definition.
fn needs_refresh(run_key: Option<&str>, task: Option<&str>, wanted: &str) -> bool {
    run_key.is_some() || task.is_some_and(|c| c != wanted)
}

/// Move an old Run entry to the logon task, and keep the task pointing at
/// this copy if the exe was moved / updated.
pub fn refresh_autostart() {
    let Ok(wanted) = autostart_command() else {
        return;
    };
    let (run_key, task) = (imp::get_run_key(), imp::get_task());
    if needs_refresh(run_key.as_deref(), task.as_deref(), &wanted) {
        let _ = set_autostart(true);
    }
}

/// False if another copy is already running (both would fight over the
/// same audio devices).  The named mutex lives until this process exits.
/// Always true outside Windows.
pub fn claim_single_instance() -> bool {
    imp::claim_single_instance()
}

/// Open Windows' Sound settings page (`ms-settings:sound`).
pub fn open_sound_settings() -> Result<()> {
    imp::shell_open(std::ffi::OsStr::new("ms-settings:sound"))
}

/// Open a folder (or file) in Explorer.
pub fn open_folder(path: &Path) -> Result<()> {
    imp::shell_open(path.as_os_str())
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    pub fn get_task() -> Option<String> {
        None
    }
    pub fn get_run_key() -> Option<String> {
        None
    }
    pub fn register_task(_xml: &str) -> Result<()> {
        bail!("autostart is only supported on Windows")
    }
    pub fn delete_task() -> Result<()> {
        bail!("autostart is only supported on Windows")
    }
    pub fn delete_run_key() -> Result<()> {
        Ok(())
    }
    pub fn claim_single_instance() -> bool {
        true
    }
    pub fn shell_open(_target: &std::ffi::OsStr) -> Result<()> {
        bail!("only supported on Windows")
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use anyhow::{bail, Context, Result};
    use windows::core::{BSTR, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{
        GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, HANDLE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ,
    };
    use windows::Win32::System::TaskScheduler::{
        ITaskFolder, ITaskService, TaskScheduler, TASK_CREATE_OR_UPDATE,
        TASK_LOGON_INTERACTIVE_TOKEN,
    };
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{task_command, MUTEX_NAME, RUN_KEY, RUN_NAME, TASK_NAME};

    /// COM for this call (see vcable.rs: a GUI thread that already chose
    /// another apartment fails with RPC_E_CHANGED_MODE and COM works as is).
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

    /// The Task Scheduler root folder, where the logon task lives.
    fn root_folder() -> Result<ITaskFolder> {
        let none = VARIANT::default();
        // SAFETY: documented Task Scheduler 2.0 activation and calls
        unsafe {
            let svc: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
                .context("cannot reach Task Scheduler")?;
            svc.Connect(&none, &none, &none, &none)
                .context("cannot connect to Task Scheduler")?;
            Ok(svc.GetFolder(&BSTR::from("\\"))?)
        }
    }

    /// Command line of our logon task ("" for an older definition), None if
    /// there is none.
    pub fn get_task() -> Option<String> {
        get_task_named(TASK_NAME)
    }

    pub fn register_task(xml: &str) -> Result<()> {
        register_task_named(TASK_NAME, xml)
    }

    pub fn delete_task() -> Result<()> {
        delete_task_named(TASK_NAME)
    }

    pub fn get_task_named(name: &str) -> Option<String> {
        let _com = Com::init();
        // SAFETY: plain COM calls on interfaces we own
        let xml = unsafe {
            root_folder()
                .ok()?
                .GetTask(&BSTR::from(name))
                .ok()?
                .Xml()
                .ok()?
        };
        Some(task_command(&xml.to_string()))
    }

    pub fn register_task_named(name: &str, xml: &str) -> Result<()> {
        let _com = Com::init();
        let none = VARIANT::default();
        // SAFETY: plain COM call; empty user / password with an interactive
        // token means "the signed-in user", no admin rights needed
        unsafe {
            root_folder()?
                .RegisterTask(
                    &BSTR::from(name),
                    &BSTR::from(xml),
                    TASK_CREATE_OR_UPDATE.0,
                    &none,
                    &none,
                    TASK_LOGON_INTERACTIVE_TOKEN,
                    &none,
                )
                .context("cannot create the sign-in task")?;
        }
        Ok(())
    }

    pub fn delete_task_named(name: &str) -> Result<()> {
        let _com = Com::init();
        // SAFETY: plain COM call
        let r = unsafe { root_folder()?.DeleteTask(&BSTR::from(name), 0) };
        match r {
            Err(e) if e.code() != ERROR_FILE_NOT_FOUND.to_hresult() => {
                Err(e).context("cannot delete the sign-in task")
            }
            _ => Ok(()),
        }
    }

    pub fn delete_run_key() -> Result<()> {
        let key = HSTRING::from(RUN_KEY);
        let name = HSTRING::from(RUN_NAME);
        // SAFETY: plain registry call with valid strings
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &name) };
        if r.is_err() && r != ERROR_FILE_NOT_FOUND {
            r.ok().context("cannot delete the Run registry value")?;
        }
        Ok(())
    }

    pub fn get_run_key() -> Option<String> {
        let key = HSTRING::from(RUN_KEY);
        let name = HSTRING::from(RUN_NAME);
        // SAFETY: first call asks for the size in bytes, second fills a
        // buffer of that size; RegGetValueW NUL-terminates REG_SZ data
        unsafe {
            let mut size = 0u32;
            RegGetValueW(
                HKEY_CURRENT_USER,
                &key,
                &name,
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            )
            .ok()
            .ok()?;
            let mut buf = vec![0u16; (size as usize).div_ceil(2)];
            RegGetValueW(
                HKEY_CURRENT_USER,
                &key,
                &name,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
            .ok()
            .ok()?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            Some(String::from_utf16_lossy(&buf[..len]))
        }
    }

    /// The mutex handle, kept open (never closed) until the process exits.
    static MUTEX: OnceLock<isize> = OnceLock::new();

    pub fn claim_single_instance() -> bool {
        if MUTEX.get().is_some() {
            return true; // already claimed by this process
        }
        // SAFETY: CreateMutexW with a constant name; GetLastError is read
        // right after it, as the API requires
        unsafe {
            let Ok(h): windows::core::Result<HANDLE> =
                CreateMutexW(None, false, &HSTRING::from(MUTEX_NAME))
            else {
                return true; // could not check; do not block the user
            };
            let existed = GetLastError() == ERROR_ALREADY_EXISTS;
            let _ = MUTEX.set(h.0 as isize);
            !existed
        }
    }

    pub fn shell_open(target: &std::ffi::OsStr) -> Result<()> {
        let target = HSTRING::from(target);
        // SAFETY: valid NUL-terminated strings; ShellExecuteW returns a
        // value > 32 on success
        let r = unsafe {
            ShellExecuteW(
                None,
                &HSTRING::from("open"),
                &target,
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if r.0 as isize <= 32 {
            bail!("cannot open {target} (ShellExecute error {})", r.0 as isize);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_command_format() {
        let p = Path::new(r"C:\Program Files\DeskEchoGuard\desk-echo-guard.exe");
        assert_eq!(
            autostart_command_for(p),
            r#""C:\Program Files\DeskEchoGuard\desk-echo-guard.exe" --autostart"#
        );
    }

    #[test]
    fn refresh_when_run_key_left_or_task_points_elsewhere() {
        assert!(!needs_refresh(None, None, "x"));
        assert!(!needs_refresh(None, Some("x"), "x"));
        assert!(needs_refresh(None, Some("old"), "x"));
        assert!(needs_refresh(None, Some(""), "x")); // older task definition
        assert!(needs_refresh(Some("x"), None, "x")); // move Run key -> task
    }

    #[test]
    fn task_xml_roundtrips_to_the_command() {
        let p = Path::new(r"C:\Tools & Apps\DeskEchoGuard.exe");
        let xml = task_xml(p, r"PC\sam");
        assert!(xml.contains("<Priority>3</Priority>"));
        assert!(xml.contains(r"<UserId>PC\sam</UserId>"));
        assert_eq!(task_command(&xml), autostart_command_for(p));
        // a task another version wrote at the default priority is rewritten
        assert_eq!(
            task_command(&xml.replace("<Priority>3<", "<Priority>7<")),
            ""
        );
    }

    #[cfg(windows)]
    #[test]
    fn logon_task_register_read_delete() {
        // a throwaway name: never touch the installed copy's real autostart
        let name = "DeskEchoGuardTest";
        let xml = task_xml(Path::new(r"C:\x\DeskEchoGuard.exe"), &current_user());
        imp::register_task_named(name, &xml).unwrap();
        let got = imp::get_task_named(name);
        imp::delete_task_named(name).unwrap();
        assert_eq!(
            got.as_deref(),
            Some(r#""C:\x\DeskEchoGuard.exe" --autostart"#)
        );
        assert_eq!(imp::get_task_named(name), None);
        imp::delete_task_named(name).unwrap(); // already gone: not an error
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_stubs() {
        assert_eq!(get_autostart(), None);
        assert!(set_autostart(true).is_err());
        assert!(claim_single_instance());
        refresh_autostart();
    }
}
