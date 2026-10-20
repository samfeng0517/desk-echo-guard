//! Desk Echo Guard (Windows): GUI entry point.
//!
//! No arguments, `--config FILE` or `--autostart` -> GUI.
//! `--screenshot DIR` -> render all pages to PNG (dev / docs).
//!
//! TODO(wiring): dispatch before the GUI starts:
//!   * `--vcable ACTIONS RESULT.json` -> `echoguard_core::vcable::elevated_main`
//!     (the UAC-elevated helper started by `vcable::run_elevated`)
//!   * `--self-test [report.txt]`     -> `echoguard_core::selftest`
//!   * any other option of `cli.py` (clap) -> console mode; on Windows call
//!     `AttachConsole(ATTACH_PARENT_PROCESS)` first, as this exe uses the
//!     GUI subsystem.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

slint::include_modules!();

mod engine;
mod mock;
mod tray;
mod ui;

const USAGE: &str = "usage: DeskEchoGuard [--config FILE] [--autostart] [--screenshot DIR]";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut config: Option<PathBuf> = None;
    let mut autostart = false;
    let mut screenshot: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => match args.next() {
                Some(p) => config = Some(p.into()),
                None => return usage_error("--config needs a file"),
            },
            "--autostart" => autostart = true,
            "--screenshot" => match args.next() {
                Some(p) => screenshot = Some(p.into()),
                None => return usage_error("--screenshot needs a directory"),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            // TODO(wiring): hand everything else to the clap CLI (cli.py options)
            other => return usage_error(&format!("unknown option {other}")),
        }
    }

    let res = match screenshot {
        Some(dir) => ui::screenshot::run(&dir),
        None => ui::run_gui(ui::GuiOptions { config, autostart }),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("DeskEchoGuard: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("DeskEchoGuard: {msg}\n{USAGE}");
    ExitCode::from(2)
}
