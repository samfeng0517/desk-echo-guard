//! `DeskEchoGuard --screenshot DIR`: render every page (light and dark,
//! plus tooltip / InfoBar / dialog examples) to PNG headlessly with Slint's
//! software renderer and the mock engine.  Used for review and the docs
//! (rust/docs/images).  Time is simulated, so hover delays and animations
//! are deterministic.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::Rgb8Pixel;
use slint::{ComponentHandle, LogicalPosition, PhysicalSize};

use super::controller::{Controller, Options, StartReason};
use crate::engine::Engine;
use crate::mock::{MockEngine, MockOptions};
use crate::{AppState, AppWindow, Cfg};

struct ShotPlatform {
    window: Rc<MinimalSoftwareWindow>,
    now: Rc<Cell<Duration>>,
}

impl Platform for ShotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.now.get()
    }
}

struct Shooter {
    ui: AppWindow,
    window: Rc<MinimalSoftwareWindow>,
    now: Rc<Cell<Duration>>,
    dir: PathBuf,
    ctrl: Option<Rc<Controller>>,
}

impl Shooter {
    fn advance(&self, ms: u64) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(16);
            self.now.set(self.now.get() + Duration::from_millis(step));
            slint::platform::update_timers_and_animations();
            left -= step;
        }
    }

    fn size(&self, w: u32, h: u32) {
        self.ui.window().set_size(PhysicalSize::new(w, h));
        self.advance(50);
    }

    fn engine(&mut self, opts: MockOptions) {
        if let Some(c) = self.ctrl.take() {
            c.shutdown();
        }
        let engine: Arc<dyn Engine> = Arc::new(MockEngine::new(opts, Arc::new(|_| {})));
        let ctrl = Controller::install(
            &self.ui,
            engine,
            Options {
                config_path: Some(self.dir.join("screenshot-settings.toml")),
                autostart: false,
                use_tray: false,
                sync: true,
            },
        );
        ctrl.set_visible(true);
        self.ctrl = Some(ctrl);
    }

    fn ctrl(&self) -> &Rc<Controller> {
        self.ctrl.as_ref().expect("engine() first")
    }

    fn page(&self, p: i32) {
        let app = self.ui.global::<AppState>();
        app.set_page(p);
        app.invoke_page_shown(p);
        self.advance(400);
    }

    fn hover(&self, x: f32, y: f32) {
        self.window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, y),
        });
        self.advance(800);
    }

    fn unhover(&self) {
        self.window.dispatch_event(WindowEvent::PointerExited);
        self.advance(300);
    }

    fn shot(&self, name: &str) -> Result<()> {
        self.advance(300);
        self.window.request_redraw();
        let size = self.window.size();
        let (w, h) = (size.width as usize, size.height as usize);
        let mut buf = vec![Rgb8Pixel::default(); w * h];
        self.window.draw_if_needed(|r| {
            r.render(&mut buf, w);
        });
        let path = self.dir.join(format!("{name}.png"));
        write_png(&path, &buf, w as u32, h as u32)?;
        let kb = std::fs::metadata(&path)?.len() / 1024;
        println!("{} ({w}x{h}, {kb} KB)", path.display());
        Ok(())
    }
}

fn write_png(path: &Path, px: &[Rgb8Pixel], w: u32, h: u32) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let mut writer = enc.write_header()?;
    let raw: Vec<u8> = px.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    writer.write_image_data(&raw)?;
    Ok(())
}

pub fn run(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let now = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(ShotPlatform {
        window: window.clone(),
        now: now.clone(),
    }))
    .map_err(|e| anyhow::anyhow!("set_platform: {e:?}"))?;

    let ui = AppWindow::new()?;
    ui.set_font_family(super::font_family().into());
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
    ui.show()?;
    let mut sh = Shooter {
        ui,
        window,
        now,
        dir: dir.to_path_buf(),
        ctrl: None,
    };
    let _ = std::fs::remove_file(sh.dir.join("screenshot-settings.toml"));

    for dark in [false, true] {
        let suffix = if dark { "dark" } else { "light" };
        sh.ui.invoke_set_dark(dark);
        sh.size(900, 640);
        sh.engine(MockOptions {
            delays: false,
            ..Default::default()
        });
        sh.ctrl().start(StartReason::User);
        std::thread::sleep(Duration::from_millis(1300)); // let the mock levels move
        sh.page(0);
        sh.shot(&format!("home-{suffix}"))?;
        sh.page(1);
        sh.shot(&format!("devices-{suffix}"))?;
        // Siri page with the guard switched on
        sh.ui.global::<Cfg>().set_siri_guard(true);
        sh.ui.global::<Cfg>().invoke_changed();
        sh.page(2);
        sh.shot(&format!("siri-{suffix}"))?;
        sh.ui.global::<Cfg>().set_siri_guard(false);
        sh.ui.global::<Cfg>().invoke_changed();
        sh.page(3);
        sh.shot(&format!("advanced-{suffix}"))?;
    }

    // ---- light-theme extras
    sh.ui.invoke_set_dark(false);
    sh.size(900, 640);
    sh.engine(MockOptions {
        delays: false,
        ..Default::default()
    });
    sh.page(0);
    // tooltip on the start button
    let (x, y) = std::env::var("DEG_SHOT_HOVER")
        .ok()
        .and_then(|v| {
            v.split_once(',')
                .map(|(a, b)| (a.parse().ok(), b.parse().ok()))
        })
        .and_then(|(a, b)| Some((a?, b?)))
        .unwrap_or((790.0, 136.0));
    sh.hover(x, y);
    sh.shot("home-tooltip-light")?;
    sh.unhover();

    // narrow window (minimum size): navigation collapses to icons
    sh.size(760, 560);
    sh.page(0);
    sh.shot("home-narrow-light")?;
    sh.size(900, 640);

    // InfoBars: mic too loud, Siri guard without VB-CABLE A+B
    sh.engine(MockOptions {
        delays: false,
        loud_mic: true,
        cable_ab: false,
        ..Default::default()
    });
    sh.ui.global::<Cfg>().set_siri_guard(true);
    sh.ui.global::<Cfg>().invoke_changed();
    sh.ctrl().start(StartReason::User);
    sh.advance(1200);
    sh.page(0);
    sh.shot("home-infobars-light")?;
    sh.ui.global::<Cfg>().set_siri_guard(false);
    sh.ui.global::<Cfg>().invoke_changed();

    sh.engine(MockOptions {
        delays: false,
        cable: false,
        cable_ab: false,
        ..Default::default()
    });
    sh.page(1);
    sh.shot("devices-no-cable-light")?;

    // VB-Cable dialog
    sh.engine(MockOptions {
        delays: false,
        ..Default::default()
    });
    sh.page(1);
    let app = sh.ui.global::<AppState>();
    app.set_dialog_rename(true);
    app.set_dialog_hide(true);
    app.set_dialog(1);
    sh.shot("devices-dialog-light")?;
    app.set_dialog(0);

    // Advanced, scrolled content: calibration result + diagnostics, tall
    sh.size(900, 2450);
    app.invoke_calibrate();
    app.set_diag_expanded(true);
    app.set_about_expanded(true);
    sh.ctrl().start(StartReason::User);
    sh.page(3);
    sh.shot("advanced-full-light")?;
    sh.ui.invoke_set_dark(true);
    sh.shot("advanced-full-dark")?;

    if let Some(c) = sh.ctrl.take() {
        c.shutdown();
    }
    let _ = std::fs::remove_file(sh.dir.join("screenshot-settings.toml"));
    Ok(())
}
