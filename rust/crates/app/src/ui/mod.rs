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
    select_renderer();
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

/// Renderer choice.  The software renderer (partial repaints) is the
/// default: measured under Xvfb it needs ~3 % CPU and ~45 MB RSS while the
/// meters move, against ~40 % / ~155 MB for the OpenGL (femtovg) renderer.
/// It cannot draw a transparent window, so Mica needs the GPU renderer:
/// `DEG_MICA=1` opts in.  `SLINT_BACKEND` still overrides both.
fn use_gpu_renderer() -> bool {
    std::env::var_os("DEG_MICA").is_some_and(|v| v != "0")
}

fn select_renderer() {
    if std::env::var_os("SLINT_BACKEND").is_some() {
        return;
    }
    let renderer = if use_gpu_renderer() {
        "femtovg"
    } else {
        "software"
    };
    if let Err(e) = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.into())
        .select()
    {
        log::warn!("renderer {renderer}: {e}");
    }
}

/// Called every time the window becomes visible: Mica on Windows 11
/// (only with the GPU renderer, see [`use_gpu_renderer`]).
pub fn window_shown(ui: &AppWindow) {
    #[cfg(windows)]
    {
        use std::cell::Cell;
        thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
        if DONE.with(|d| d.replace(true)) || !use_gpu_renderer() {
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
