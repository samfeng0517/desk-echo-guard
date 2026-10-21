//! Windows helpers for the GUI (`winutil.py`): "start with Windows" (HKCU
//! Run key), single instance, and opening the Sound settings / a folder.
//! Elsewhere they do nothing or return an error.

use std::path::Path;

use anyhow::Result;

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_NAME: &str = "DeskEchoGuard";
pub const AUTOSTART_FLAG: &str = "--autostart";
pub const MUTEX_NAME: &str = r"Local\DeskEchoGuard.SingleInstance";

/// Run-key value for this exe: open the GUI and start right away.
pub fn autostart_command_for(exe: &Path) -> String {
    format!("\"{}\" {AUTOSTART_FLAG}", exe.display())
}

/// Command line Windows runs at sign-in (this exe + `--autostart`).
pub fn autostart_command() -> Result<String> {
    Ok(autostart_command_for(&std::env::current_exe()?))
}

/// The current Run-key value, None if autostart is off (or not Windows).
pub fn get_autostart() -> Option<String> {
    imp::get_autostart()
}

/// Turn "start with Windows" on (pointing at this exe) or off.
pub fn set_autostart(enabled: bool) -> Result<()> {
    imp::set_autostart(enabled)
}

/// Whether the Run entry must be rewritten: it exists but points elsewhere.
fn needs_refresh(current: Option<&str>, wanted: &str) -> bool {
    current.is_some_and(|c| c != wanted)
}

/// Keep the Run entry pointing at this copy if the exe was moved / updated.
pub fn refresh_autostart() {
    let Ok(wanted) = autostart_command() else {
        return;
    };
    if needs_refresh(get_autostart().as_deref(), &wanted) {
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

    pub fn get_autostart() -> Option<String> {
        None
    }
    pub fn set_autostart(_enabled: bool) -> Result<()> {
        bail!("autostart is only supported on Windows")
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
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{
        GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, HANDLE,
    };
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{autostart_command, MUTEX_NAME, RUN_KEY, RUN_NAME};

    pub fn get_autostart() -> Option<String> {
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

    pub fn set_autostart(enabled: bool) -> Result<()> {
        let key = HSTRING::from(RUN_KEY);
        let name = HSTRING::from(RUN_NAME);
        if enabled {
            let cmd = HSTRING::from(autostart_command()?);
            // REG_SZ data: UTF-16 including the terminating NUL
            let bytes = ((cmd.len() + 1) * 2) as u32;
            // SAFETY: `cmd` is a NUL-terminated wide string of `bytes` bytes
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    &key,
                    &name,
                    REG_SZ.0,
                    Some(cmd.as_ptr().cast()),
                    bytes,
                )
                .ok()
                .context("cannot write the Run registry key")?;
            }
        } else {
            // SAFETY: plain registry call with valid strings
            let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &name) };
            if r.is_err() && r != ERROR_FILE_NOT_FOUND {
                r.ok().context("cannot delete the Run registry value")?;
            }
        }
        Ok(())
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
    fn refresh_only_when_entry_points_elsewhere() {
        assert!(!needs_refresh(None, "x"));
        assert!(!needs_refresh(Some("x"), "x"));
        assert!(needs_refresh(Some("old"), "x"));
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
