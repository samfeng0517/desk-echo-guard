//! GUI entry points.

pub mod controller;
pub mod screenshot;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use slint::ComponentHandle;

use crate::engine::Engine;
use crate::mock::{MockEngine, MockOptions};
use crate::{AppWindow, Theme};
use controller::{Controller, Msg, Options};

pub struct GuiOptions {
    pub config: Option<PathBuf>,
    pub autostart: bool,
}

/// Font that covers Traditional Chinese on each platform.
pub fn font_family() -> &'static str {
    if cfg!(windows) {
        "Microsoft JhengHei UI"
    } else {
        "Noto Sans CJK TC"
    }
}

/// The engine the GUI runs on.
/// TODO(wiring): return `CoreEngine` (echoguard-core) here; keep MockEngine
/// behind an env var / dev flag for screenshots and UI work.
pub fn make_engine() -> Arc<dyn Engine> {
    let log = Arc::new(|line: String| controller::post(Msg::Log(line)));
    Arc::new(MockEngine::new(MockOptions::from_env(), log))
}

pub fn run_gui(opts: GuiOptions) -> Result<()> {
    // TODO(wiring): single instance (winutil::claim_single_instance): if
    // another copy runs, show "Desk Echo Guard 已經在執行中…" and return
    // (silently with --autostart).  TODO(wiring): winutil::refresh_autostart().
    let ui = AppWindow::new()?;
    ui.set_font_family(font_family().into());
    let ctrl = Controller::install(
        &ui,
        make_engine(),
        Options {
            config_path: opts.config,
            autostart: opts.autostart,
            use_tray: true,
            sync: false,
        },
    );
    if !opts.autostart {
        // launched at sign-in: stay in the tray, start automatically
        ctrl.show_window();
    }
    slint::run_event_loop_until_quit()?;
    ctrl.shutdown();
    Ok(())
}

/// Called every time the window becomes visible: Mica on Windows 11.
pub fn window_shown(ui: &AppWindow) {
    #[cfg(windows)]
    {
        use std::cell::Cell;
        thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
        if DONE.with(|d| d.replace(true)) || std::env::var_os("DEG_NO_MICA").is_some() {
            return;
        }
        let handle = ui.window().window_handle();
        // best effort: Windows 10 and older builds return an error and keep
        // the solid background
        // None: Mica follows the system light / dark setting
        if window_vibrancy::apply_mica(&handle, None).is_ok() {
            ui.global::<Theme>().set_mica(true);
        }
    }
    #[cfg(not(windows))]
    let _ = ui.global::<Theme>();
}
