//! Desk Echo Guard (Windows): entry point.
//!
//! * no arguments, `--config FILE`, `--autostart` -> GUI
//! * any option of `cli.py` -> console mode (see `cli.rs`)
//! * `--self-test [report.txt]` -> self-test, exit code 0 = pass
//! * `--vcable ACTIONS RESULT.json` -> UAC-elevated VB-Cable helper
//! * `--screenshot DIR` -> render all pages to PNG (dev / docs, mock engine)
//!
//! The exe uses the GUI subsystem, so console output needs
//! [`attach_console`] first.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

use clap::Parser;

slint::include_modules!();

mod cli;
mod core_engine;
mod engine;
mod mock;
mod tray;
mod ui;
mod wav;

use cli::{Cli, Mode};

fn main() -> ExitCode {
    let args = match Cli::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // --help or a usage error: needs a console to be seen
            attach_console();
            e.exit();
        }
    };
    let mode = args.mode();
    if !matches!(mode, Mode::Gui { .. }) {
        attach_console();
    }
    let code = match mode {
        Mode::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            0
        }
        Mode::Vcable(a) => echoguard_core::vcable::elevated_main(&a),
        Mode::SelfTest(report) => cli::self_test(report.as_deref()),
        Mode::Console => match cli::run(&args) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("DeskEchoGuard: {e:#}");
                1
            }
        },
        Mode::Screenshot(dir) => report(ui::screenshot::run(&dir)),
        Mode::Gui { config, autostart } => {
            report(ui::run_gui(ui::GuiOptions { config, autostart }))
        }
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn report(res: anyhow::Result<()>) -> i32 {
    match res {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("DeskEchoGuard: {e:#}");
            1
        }
    }
}

/// Make println! / eprintln! reach the cmd / PowerShell window this GUI
/// subsystem exe was started from.  Redirected handles (`> file`, pipes,
/// `Start-Process -RedirectStandardOutput`) are kept; without a parent
/// console (double-click, plain `Start-Process`) output is dropped silently
/// and `--self-test REPORT` still writes its file.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_HANDLE, STD_OUTPUT_HANDLE,
    };

    fn usable(which: STD_HANDLE) -> bool {
        // SAFETY: plain query of this process's standard handle.
        let h = unsafe { GetStdHandle(which) };
        !h.is_null() && h != INVALID_HANDLE_VALUE
    }

    let need = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .filter(|&w| !usable(w))
        .collect::<Vec<_>>();
    if need.is_empty() {
        return; // both redirected: nothing to do
    }
    // SAFETY: FFI calls with valid arguments; the CONOUT$ handle is owned by
    // the process standard-handle table for the rest of the run.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let name: Vec<u16> = "CONOUT$".encode_utf16().chain([0]).collect();
        let h = CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return;
        }
        for w in need {
            SetStdHandle(w, h);
        }
    }
    // the shell already printed its prompt: start on a fresh line
    println!();
}

#[cfg(not(windows))]
fn attach_console() {}
