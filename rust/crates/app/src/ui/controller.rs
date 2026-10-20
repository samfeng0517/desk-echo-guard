//! All GUI state <-> engine glue: settings auto-save, auto-apply (debounced
//! restart), status polling, InfoBars, VB-Cable, calibration, tray, close
//! to tray.  The engine is only reached through [`Engine`]; swapping the
//! mock for echoguard-core is a change in `ui::run_gui` only.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::engine::{
    CableAction, CableStatus, DeviceInfo, DeviceKind, Engine, LogSink, Settings, Status,
    CABLE_RENAMES, VBCABLE_DOWNLOAD_URL,
};
use crate::tray::{Tray, TrayCmd};
use crate::{AppState, AppWindow, Cfg, DiagRow, InfoBarData, Severity};

pub const ENGINES: [&str; 5] = ["auto", "webrtc", "speex", "nlms", "off"];
pub const MODES: [&str; 3] = ["always", "sibilant", "detect"];
pub const PROFILES: [&str; 4] = ["deep-wide", "apple-deep", "apple-wide", "formant"];

const LOG_LINES: usize = 300;
const APPLY_DELAY: Duration = Duration::from_millis(800);
const SAVE_DELAY: Duration = Duration::from_millis(400);
const POLL_FAST: Duration = Duration::from_millis(150);
const POLL_SLOW: Duration = Duration::from_millis(1000);
const AUTO_START_TRIES: u32 = 12;
const SOUND_SETTINGS_URL: &str = "ms-settings:sound";

/// Messages from worker threads / the tray, handled on the UI thread.
pub enum Msg {
    Log(String),
    Started(Result<(), String>, StartReason),
    Stopped(AfterStop),
    Calibrated(Result<(f64, f64), String>, bool),
    CableDone((bool, String), bool),
    Tray(TrayCmd),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StartReason {
    User,
    /// settings changed while running
    Apply,
    /// after calibration / VB-Cable changes
    Resume,
    /// launched at sign-in; attempt number
    Auto(u32),
}

pub enum AfterStop {
    Nothing,
    Apply,
    Quit,
    Calibrate,
    Cable(Vec<CableAction>),
}

#[derive(Clone)]
struct CalibResult {
    severity: Severity,
    title: String,
    message: String,
    /// (delay_ms, mic_delay_ms) to apply
    suggestion: Option<(f64, f64)>,
}

#[derive(Default)]
struct State {
    settings: Settings,
    config_path: PathBuf,
    running: bool,
    busy: bool,
    visible: bool,
    devices: Vec<DeviceInfo>,
    cable: CableStatus,
    generation: u64,
    mic_opts: Vec<Option<String>>,
    spk_opts: Vec<Option<String>>,
    out_opts: Vec<String>,
    call_opts: Vec<String>,
    log: VecDeque<String>,
    log_dirty: bool,
    loud_until: Option<Instant>,
    pipeline_error: Option<String>,
    start_error: Option<String>,
    autostart_failed: bool,
    autostart_error: Option<String>,
    cable_result: Option<(bool, String)>,
    calib: Option<CalibResult>,
    calibrating: bool,
    tray_hint_shown: bool,
    apply_pending: bool,
    poll_fast: bool,
    toast_until: Option<Instant>,
    started_at: Option<Instant>,
}

pub struct Controller {
    ui: slint::Weak<AppWindow>,
    engine: Arc<dyn Engine>,
    st: RefCell<State>,
    tray: RefCell<Option<Tray>>,
    clipboard: RefCell<Option<arboard::Clipboard>>,
    poll: Timer,
    apply_timer: Timer,
    save_timer: Timer,
    retry_timer: Timer,
    t0: Instant,
    /// run worker jobs inline (headless screenshots)
    sync: bool,
}

thread_local! {
    static CTRL: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
}

fn with_ctrl(f: impl FnOnce(&Rc<Controller>)) {
    let c = CTRL.with(|c| c.borrow().clone());
    if let Some(c) = c {
        f(&c);
    }
}

/// Deliver a message to the controller on the UI thread (from any thread).
pub fn post(msg: Msg) {
    let _ = slint::invoke_from_event_loop(move || with_ctrl(|c| c.handle(msg)));
}

fn s(x: impl Into<SharedString>) -> SharedString {
    x.into()
}

fn bar(severity: Severity, title: &str, message: &str, action: &str, id: &str, tip: &str) -> InfoBarData {
    InfoBarData {
        severity,
        title: s(title),
        message: s(message),
        action: s(action),
        action_id: s(id),
        tip: s(tip),
    }
}

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

fn is_vbcable(name: &str) -> bool {
    name.to_lowercase().contains("vb-audio")
}

/// Configured device query -> index in `names` (exact, then substring).
fn match_index(names: &[String], query: &str) -> Option<usize> {
    let q = query.to_lowercase();
    names
        .iter()
        .position(|n| n.to_lowercase() == q)
        .or_else(|| names.iter().position(|n| n.to_lowercase().contains(&q)))
}

fn db_level(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

pub struct Options {
    pub config_path: Option<PathBuf>,
    pub autostart: bool,
    pub use_tray: bool,
    pub sync: bool,
}

impl Controller {
    pub fn install(ui: &AppWindow, engine: Arc<dyn Engine>, opts: Options) -> Rc<Controller> {
        let config_path = opts
            .config_path
            .unwrap_or_else(|| engine.default_config_path());
        let ctrl = Rc::new(Controller {
            ui: ui.as_weak(),
            engine,
            st: RefCell::new(State {
                config_path,
                ..Default::default()
            }),
            tray: RefCell::new(None),
            clipboard: RefCell::new(None),
            poll: Timer::default(),
            apply_timer: Timer::default(),
            save_timer: Timer::default(),
            retry_timer: Timer::default(),
            t0: Instant::now(),
            sync: opts.sync,
        });
        CTRL.with(|c| *c.borrow_mut() = Some(ctrl.clone()));
        ctrl.bind_callbacks(ui);
        ctrl.load_settings();

        let app = ui.global::<AppState>();
        app.set_version(s(env!("CARGO_PKG_VERSION")));
        app.set_is_windows(cfg!(windows));
        app.set_autostart_available(ctrl.engine.autostart_supported());
        app.set_autostart(ctrl.engine.autostart_enabled());
        app.set_config_path(s(ctrl.st.borrow().config_path.display().to_string()));

        ctrl.refresh_devices();
        ctrl.write_cfg();
        ctrl.refresh_diag(None);
        ctrl.refresh_bars();
        ctrl.set_poll(false);

        if opts.use_tray {
            // tray-icon wants a running event loop on this thread
            let weak = Rc::downgrade(&ctrl);
            Timer::single_shot(Duration::from_millis(0), move || {
                if let Some(c) = weak.upgrade() {
                    match Tray::new(|cmd| post(Msg::Tray(cmd))) {
                        Ok(t) => *c.tray.borrow_mut() = t,
                        Err(e) => c.log(format!("無法建立系統匣圖示：{e:#}")),
                    }
                }
            });
        }
        if opts.autostart {
            let weak = Rc::downgrade(&ctrl);
            ctrl.retry_timer.start(TimerMode::SingleShot, Duration::from_secs(1), move || {
                if let Some(c) = weak.upgrade() {
                    c.start(StartReason::Auto(1));
                }
            });
        }
        ctrl
    }

    fn ui(&self) -> Option<AppWindow> {
        self.ui.upgrade()
    }

    // ------------------------------------------------------------ binding

    fn bind_callbacks(&self, ui: &AppWindow) {
        let app = ui.global::<AppState>();
        app.on_toggle_run(|| with_ctrl(|c| c.toggle_run()));
        app.on_infobar_action(|id| with_ctrl(|c| c.infobar_action(&id)));
        app.on_copy_text(|t| with_ctrl(|c| c.copy(&t)));
        app.on_open_url(|u| with_ctrl(|c| c.open_url(&u)));
        app.on_page_shown(|p| {
            with_ctrl(|c| {
                if p == 1 || p == 0 {
                    c.refresh_devices();
                }
                c.refresh_bars();
            })
        });
        app.on_tidy_cable(|rename, hide| {
            with_ctrl(|c| {
                let mut a = Vec::new();
                if rename {
                    a.push(CableAction::Rename);
                }
                if hide {
                    a.push(CableAction::Hide);
                }
                c.cable_actions(a)
            })
        });
        app.on_restore_cable(|| with_ctrl(|c| c.cable_actions(vec![CableAction::Restore])));
        app.on_calibrate(|| with_ctrl(|c| c.calibrate()));
        app.on_set_autostart(|on| with_ctrl(|c| c.set_autostart(on)));
        app.on_copy_log(|| {
            with_ctrl(|c| {
                let text = c.st.borrow().log.iter().cloned().collect::<Vec<_>>().join("\n");
                c.copy_quiet(&text, "已複製執行記錄");
            })
        });
        app.on_open_config_folder(|| {
            with_ctrl(|c| {
                let dir = c.st.borrow().config_path.parent().map(PathBuf::from);
                if let Some(dir) = dir {
                    let _ = std::fs::create_dir_all(&dir);
                    if let Err(e) = open::that_detached(&dir) {
                        c.log(format!("無法開啟資料夾：{e}"));
                    }
                }
            })
        });
        app.on_pick_model(|| with_ctrl(|c| c.pick_model()));
        app.on_pick_debug_folder(|| with_ctrl(|c| c.pick_debug_folder()));
        ui.global::<Cfg>().on_changed(|| with_ctrl(|c| c.read_cfg()));

        ui.window().on_close_requested(|| {
            let mut resp = slint::CloseRequestResponse::HideWindow;
            with_ctrl(|c| {
                if !c.close_to_tray() {
                    resp = slint::CloseRequestResponse::KeepWindowShown;
                }
            });
            resp
        });
    }

    // ----------------------------------------------------------- messages

    pub fn handle(&self, msg: Msg) {
        match msg {
            Msg::Log(line) => self.log(line),
            Msg::Started(res, reason) => self.on_started(res, reason),
            Msg::Stopped(then) => self.on_stopped(then),
            Msg::Calibrated(res, resume) => self.on_calibrated(res, resume),
            Msg::CableDone(res, resume) => self.on_cable_done(res, resume),
            Msg::Tray(cmd) => match cmd {
                TrayCmd::Show => self.show_window(),
                TrayCmd::Toggle => {
                    let running = self.st.borrow().running;
                    if running {
                        self.toggle_run();
                    } else {
                        self.start(StartReason::User);
                        // let the user see why if it fails: on_started shows the window
                    }
                }
                TrayCmd::Quit => self.quit(),
            },
        }
    }

    /// Run a blocking job on a worker thread; its result comes back as a Msg.
    fn spawn(&self, job: impl FnOnce() -> Msg + Send + 'static) {
        if self.sync {
            let msg = job();
            self.handle(msg);
        } else {
            std::thread::spawn(move || post(job()));
        }
    }

    pub fn log_sink(&self) -> LogSink {
        Arc::new(|line| post(Msg::Log(line)))
    }

    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        log::info!("{line}");
        let t = self.t0.elapsed().as_secs_f64();
        let mut st = self.st.borrow_mut();
        for l in line.lines() {
            st.log.push_back(format!("[{t:7.1}s] {l}"));
        }
        while st.log.len() > LOG_LINES {
            st.log.pop_front();
        }
        st.log_dirty = true;
        drop(st);
        self.flush_log();
    }

    fn flush_log(&self) {
        let text = {
            let mut st = self.st.borrow_mut();
            if !st.log_dirty || !st.visible {
                return;
            }
            st.log_dirty = false;
            st.log.iter().cloned().collect::<Vec<_>>().join("\n")
        };
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_log_text(s(text));
        }
    }

    // ----------------------------------------------------------- settings

    fn load_settings(&self) {
        let path = self.st.borrow().config_path.clone();
        let settings = if path.exists() {
            match self.engine.load_settings(&path) {
                Ok(s) => s,
                Err(e) => {
                    self.log(format!("讀取設定檔失敗，改用預設值：{e:#}"));
                    Settings::default()
                }
            }
        } else {
            Settings::default()
        };
        self.st.borrow_mut().settings = settings;
    }

    fn save_now(&self) {
        let (path, settings) = {
            let st = self.st.borrow();
            (st.config_path.clone(), st.settings.clone())
        };
        if let Err(e) = self.engine.save_settings(&path, &settings) {
            self.log(format!("無法儲存設定：{e:#}"));
        }
    }

    fn save_soon(&self) {
        self.save_timer.start(TimerMode::SingleShot, SAVE_DELAY, || {
            with_ctrl(|c| c.save_now())
        });
    }

    /// Settings -> Cfg (all widgets).
    fn write_cfg(&self) {
        let Some(ui) = self.ui() else { return };
        let cfg = ui.global::<Cfg>();
        let st = self.st.borrow();
        let s_ = &st.settings;
        let idx = |list: &[&str], v: &str| list.iter().position(|x| *x == v).unwrap_or(0) as i32;
        cfg.set_engine_index(idx(&ENGINES, &s_.engine));
        cfg.set_noise_suppression(s_.noise_suppression);
        cfg.set_high_pass(s_.high_pass);
        cfg.set_agc(s_.agc);
        cfg.set_delay_ms(s_.delay_ms.round() as i32);
        cfg.set_mic_delay_ms(s_.mic_delay_ms.round() as i32);
        cfg.set_gain_db(s_.output_gain_db as f32);
        cfg.set_siri_guard(s_.siri_guard);
        cfg.set_siri_mode_index(idx(&MODES, &s_.siri_mode));
        let profile = if s_.siri_profile == "apple" { "deep-wide" } else { &s_.siri_profile };
        cfg.set_siri_profile_index(idx(&PROFILES, profile));
        cfg.set_siri_model(s(s_.siri_model.first().cloned().unwrap_or_default()));
        cfg.set_siri_threshold(s_.siri_threshold as f32);
        cfg.set_debug_wav(s(s_.debug_wav.clone().unwrap_or_default()));
        drop(st);
        self.write_device_indexes();
    }

    fn write_device_indexes(&self) {
        let Some(ui) = self.ui() else { return };
        let cfg = ui.global::<Cfg>();
        let st = self.st.borrow();
        let pos_opt = |opts: &[Option<String>], v: &Option<String>| {
            opts.iter().position(|o| o == v).unwrap_or(0) as i32
        };
        cfg.set_mic_index(pos_opt(&st.mic_opts, &st.settings.mic));
        cfg.set_speaker_index(pos_opt(&st.spk_opts, &st.settings.speaker));
        let out = self.engine.cable_resolve(&st.settings.out, DeviceKind::Speaker, &st.devices);
        cfg.set_out_index(st.out_opts.iter().position(|o| *o == out).unwrap_or(0) as i32);
        let call = self.engine.cable_resolve(&st.settings.call_in, DeviceKind::Mic, &st.devices);
        cfg.set_call_in_index(st.call_opts.iter().position(|o| *o == call).unwrap_or(0) as i32);
    }

    /// Cfg -> Settings, after any edit.  Saves and re-applies if running.
    fn read_cfg(&self) {
        let Some(ui) = self.ui() else { return };
        let cfg = ui.global::<Cfg>();
        let mut st = self.st.borrow_mut();
        let old = st.settings.clone();
        let mut n = old.clone();
        let pick = |list: &[&str], i: i32, fallback: &str| {
            list.get(i.max(0) as usize).copied().unwrap_or(fallback).to_string()
        };
        n.engine = pick(&ENGINES, cfg.get_engine_index(), "auto");
        n.noise_suppression = cfg.get_noise_suppression();
        n.high_pass = cfg.get_high_pass();
        n.agc = cfg.get_agc();
        n.delay_ms = cfg.get_delay_ms() as f64;
        n.mic_delay_ms = cfg.get_mic_delay_ms() as f64;
        n.output_gain_db = cfg.get_gain_db().round() as f64;
        n.siri_guard = cfg.get_siri_guard();
        n.siri_mode = pick(&MODES, cfg.get_siri_mode_index(), "always");
        n.siri_profile = pick(&PROFILES, cfg.get_siri_profile_index(), "deep-wide");
        let model = cfg.get_siri_model().to_string();
        n.siri_model = if model.is_empty() { vec![] } else { vec![model] };
        n.siri_threshold = (cfg.get_siri_threshold() as f64 * 100.0).round() / 100.0;
        let dbg = cfg.get_debug_wav().to_string();
        n.debug_wav = (!dbg.is_empty()).then_some(dbg);
        if let Some(m) = st.mic_opts.get(cfg.get_mic_index().max(0) as usize) {
            n.mic = m.clone();
        }
        if let Some(m) = st.spk_opts.get(cfg.get_speaker_index().max(0) as usize) {
            n.speaker = m.clone();
        }
        // VB-Cable endpoints: keep the configured (rename-proof) name when
        // the same device is still selected
        if let Some(o) = st.out_opts.get(cfg.get_out_index().max(0) as usize) {
            if *o != self.engine.cable_resolve(&old.out, DeviceKind::Speaker, &st.devices) {
                n.out = o.clone();
            }
        }
        if let Some(o) = st.call_opts.get(cfg.get_call_in_index().max(0) as usize) {
            if *o != self.engine.cable_resolve(&old.call_in, DeviceKind::Mic, &st.devices) {
                n.call_in = o.clone();
            }
        }
        if n == old {
            return;
        }
        st.settings = n;
        let running = st.running || st.busy;
        drop(st);
        self.save_soon();
        self.refresh_bars();
        if running {
            self.schedule_apply();
        }
    }

    fn schedule_apply(&self) {
        self.st.borrow_mut().apply_pending = true;
        self.apply_timer.start(TimerMode::SingleShot, APPLY_DELAY, || {
            with_ctrl(|c| c.apply_now())
        });
    }

    fn apply_now(&self) {
        let (busy, running) = {
            let st = self.st.borrow();
            (st.busy, st.running)
        };
        if busy {
            // try again once the current start / stop is done
            self.schedule_apply();
            return;
        }
        self.st.borrow_mut().apply_pending = false;
        if !running {
            return;
        }
        if let Some(why) = self.invalid_reason() {
            self.log(why);
            self.refresh_bars();
            return;
        }
        self.stop(AfterStop::Apply);
    }

    fn invalid_reason(&self) -> Option<String> {
        let st = self.st.borrow();
        let s_ = &st.settings;
        (s_.siri_guard && s_.siri_mode == "detect" && s_.siri_model.is_empty())
            .then(|| "「偵測到 Hey Siri 才處理」需要模型檔。".to_string())
    }

    // ------------------------------------------------------------ devices

    pub fn refresh_devices(&self) {
        let devices = match self.engine.devices() {
            Ok(d) => d,
            Err(e) => {
                self.log(format!("無法列出音效裝置：{e:#}"));
                return;
            }
        };
        let cable = self.engine.cable_status(&devices);
        let generation = self.engine.device_generation();
        let mic_name = self.engine.cable_display_name(&devices, "virtual_mic");
        let spk_name = self.engine.cable_display_name(&devices, "call_speaker");

        let names = |kind: DeviceKind, physical: bool| -> Vec<String> {
            devices
                .iter()
                .filter(|d| d.kind == kind && (!physical || !is_vbcable(&d.name)))
                .map(|d| d.name.clone())
                .collect()
        };
        let default_of = |kind: DeviceKind| {
            devices.iter().find(|d| d.kind == kind && d.default).map(|d| d.name.clone())
        };

        let mut st = self.st.borrow_mut();
        // microphones / speakers: "Windows default" + physical devices
        let mut build = |kind: DeviceKind, configured: &Option<String>| {
            let list = names(kind, true);
            let mut labels = vec![match default_of(kind) {
                Some(d) => format!("跟隨 Windows 預設（{d}）"),
                None => "跟隨 Windows 預設".to_string(),
            }];
            let mut opts = vec![None];
            for n in &list {
                labels.push(n.clone());
                opts.push(Some(n.clone()));
            }
            let mut sel = configured.clone();
            if let Some(q) = configured {
                match match_index(&list, q) {
                    Some(i) => sel = Some(list[i].clone()),
                    None => {
                        labels.push(format!("{q}（目前沒接上）"));
                        opts.push(Some(q.clone()));
                    }
                }
            }
            (labels, opts, sel)
        };
        let (mic_labels, mic_opts, mic_sel) = build(DeviceKind::Mic, &st.settings.mic);
        let (spk_labels, spk_opts, spk_sel) = build(DeviceKind::Speaker, &st.settings.speaker);
        st.settings.mic = mic_sel;
        st.settings.speaker = spk_sel;

        let mut out_opts = names(DeviceKind::Speaker, false);
        let out = self.engine.cable_resolve(&st.settings.out, DeviceKind::Speaker, &devices);
        if !out_opts.contains(&out) {
            out_opts.insert(0, out);
        }
        let mut call_opts = names(DeviceKind::Mic, false);
        let call = self.engine.cable_resolve(&st.settings.call_in, DeviceKind::Mic, &devices);
        if !call_opts.contains(&call) {
            call_opts.insert(0, call);
        }
        let mark = |opts: &[String]| -> Vec<SharedString> {
            opts.iter()
                .map(|o| {
                    if devices.iter().any(|d| d.name == *o) {
                        s(o.as_str())
                    } else {
                        s(format!("{o}（目前沒有）"))
                    }
                })
                .collect()
        };
        let out_labels = mark(&out_opts);
        let call_labels = mark(&call_opts);

        st.mic_opts = mic_opts;
        st.spk_opts = spk_opts;
        st.out_opts = out_opts;
        st.call_opts = call_opts;
        st.cable = cable.clone();
        st.devices = devices;
        st.generation = generation;
        drop(st);

        if let Some(ui) = self.ui() {
            let app = ui.global::<AppState>();
            app.set_mic_devices(model(mic_labels.into_iter().map(s).collect()));
            app.set_speaker_devices(model(spk_labels.into_iter().map(s).collect()));
            app.set_out_devices(model(out_labels));
            app.set_call_in_devices(model(call_labels));
            app.set_cable_found(cable.found);
            app.set_cable_renamed(cable.renamed);
            app.set_cable_has_ab(cable.has_ab());
            app.set_virtual_mic_name(s(mic_name.as_str()));
            app.set_siri_speaker_name(s(spk_name.as_str()));
            let status = if !cable.found {
                "沒有 VB-Cable，對方就聽不到處理過的聲音。請先安裝，裝好後重新開機。".to_string()
            } else if !cable.renamed {
                format!("Discord / LINE 的麥克風請選「{mic_name}」。建議按右邊的按鈕把名稱改得好懂一點。")
            } else if !cable.unused.is_empty() {
                format!(
                    "Discord / LINE 的麥克風請選「{mic_name}」。還有 {} 個用不到的 VB-Cable 裝置可以停用。",
                    cable.unused.len()
                )
            } else {
                format!("已安裝，Discord / LINE 的麥克風請選「{mic_name}」")
            };
            app.set_cable_status(s(status));
            app.set_rename_lines(model(
                CABLE_RENAMES.iter().map(|(a, b)| s(format!("{a} → {b}"))).collect(),
            ));
            app.set_unused_devices(model(cable.unused.iter().map(|n| s(n.as_str())).collect()));
        }
        self.write_device_indexes();
        self.refresh_bars();
    }

    // ------------------------------------------------------------ run/stop

    fn toggle_run(&self) {
        let (busy, running) = {
            let st = self.st.borrow();
            (st.busy, st.running)
        };
        if busy {
            return;
        }
        if running {
            self.stop(AfterStop::Nothing);
        } else {
            {
                let mut st = self.st.borrow_mut();
                st.pipeline_error = None;
                st.start_error = None;
                st.autostart_failed = false;
            }
            self.start(StartReason::User);
        }
    }

    fn set_busy(&self, busy: bool, text: &str) {
        self.st.borrow_mut().busy = busy;
        if let Some(ui) = self.ui() {
            let app = ui.global::<AppState>();
            app.set_busy(busy);
            app.set_busy_text(s(text));
        }
    }

    pub fn start(&self, reason: StartReason) {
        if self.st.borrow().busy {
            return;
        }
        if let Some(why) = self.invalid_reason() {
            self.st.borrow_mut().start_error = Some(why.clone());
            self.log(why);
            self.refresh_bars();
            if !matches!(reason, StartReason::Auto(_)) {
                self.show_window();
            }
            return;
        }
        self.save_now();
        self.set_busy(true, "啟動中…");
        let settings = self.st.borrow().settings.clone();
        let engine = self.engine.clone();
        self.spawn(move || {
            let res = engine.start(&settings).map_err(|e| format!("{e:#}"));
            Msg::Started(res, reason)
        });
    }

    fn on_started(&self, res: Result<(), String>, reason: StartReason) {
        self.set_busy(false, "");
        match res {
            Ok(()) => {
                {
                    let mut st = self.st.borrow_mut();
                    st.running = true;
                    st.start_error = None;
                    st.pipeline_error = None;
                    st.autostart_failed = false;
                    st.started_at = Some(Instant::now());
                }
                let engine = self.engine.engine_name().unwrap_or_default();
                self.log(format!("已開始（引擎：{engine}）。"));
                if reason == StartReason::Apply {
                    self.toast("已套用變更");
                }
            }
            Err(e) => {
                self.log(format!("無法開始：{e}"));
                if let StartReason::Auto(n) = reason {
                    if n < AUTO_START_TRIES {
                        self.log(format!("自動開始失敗，5 秒後重試（{n}/{AUTO_START_TRIES}）"));
                        self.retry_timer.start(TimerMode::SingleShot, Duration::from_secs(5), move || {
                            with_ctrl(|c| {
                                c.refresh_devices();
                                c.start(StartReason::Auto(n + 1));
                            })
                        });
                    } else {
                        self.st.borrow_mut().autostart_failed = true;
                        self.show_window();
                    }
                } else {
                    self.st.borrow_mut().start_error = Some(e);
                    self.show_window();
                }
            }
        }
        self.after_run_change();
        if self.st.borrow().apply_pending {
            self.schedule_apply();
        }
    }

    pub fn stop(&self, then: AfterStop) {
        if self.st.borrow().busy {
            return;
        }
        self.set_busy(true, "停止中…");
        let engine = self.engine.clone();
        self.spawn(move || {
            engine.stop();
            Msg::Stopped(then)
        });
    }

    fn on_stopped(&self, then: AfterStop) {
        self.set_busy(false, "");
        self.st.borrow_mut().running = false;
        self.after_run_change();
        match then {
            AfterStop::Nothing => self.log("已停止。"),
            AfterStop::Apply => self.start(StartReason::Apply),
            AfterStop::Quit => slint::quit_event_loop().unwrap_or_default(),
            AfterStop::Calibrate => self.run_calibration(true),
            AfterStop::Cable(actions) => self.run_cable(actions, true),
        }
    }

    fn after_run_change(&self) {
        let running = self.st.borrow().running;
        if let Some(ui) = self.ui() {
            let app = ui.global::<AppState>();
            app.set_running(running);
            if !running {
                app.set_mic_level(0.0);
                app.set_ref_level(0.0);
                app.set_out_level(0.0);
                for f in [AppState::set_mic_db, AppState::set_ref_db, AppState::set_out_db] {
                    f(&app, s("—"));
                }
                self.refresh_diag(None);
            }
        }
        if let Some(t) = self.tray.borrow().as_ref() {
            t.set_running(running);
        }
        self.set_poll(running);
        self.refresh_bars();
    }

    fn set_poll(&self, fast: bool) {
        let mut st = self.st.borrow_mut();
        let fast = fast && st.visible;
        if self.poll.running() && st.poll_fast == fast {
            return;
        }
        st.poll_fast = fast;
        drop(st);
        let d = if fast { POLL_FAST } else { POLL_SLOW };
        self.poll.start(TimerMode::Repeated, d, || with_ctrl(|c| c.tick()));
    }

    /// Periodic: levels, errors, clipping, device changes, toasts.
    fn tick(&self) {
        let now = Instant::now();
        let (running, visible, generation) = {
            let st = self.st.borrow();
            (st.running && !st.busy, st.visible, st.generation)
        };
        if self.engine.device_generation() != generation {
            self.refresh_devices();
        }
        let mut bars_dirty = false;
        if running {
            if let Some(status) = self.engine.status() {
                if let Some(err) = status.error.clone() {
                    self.log(format!("錯誤：{err}"));
                    self.st.borrow_mut().pipeline_error = Some(err);
                    self.stop(AfterStop::Nothing);
                    self.show_window();
                    return;
                }
                if self.engine.take_mic_peak() > 0.9 {
                    let was = self.st.borrow().loud_until.is_some();
                    self.st.borrow_mut().loud_until = Some(now + Duration::from_secs(20));
                    if !was {
                        self.log("麥克風太大聲（峰值接近滿刻度），回音會消不掉。");
                        bars_dirty = true;
                    }
                }
                if visible {
                    self.show_status(&status);
                }
            }
        }
        {
            let mut st = self.st.borrow_mut();
            if st.loud_until.is_some_and(|t| now > t) {
                st.loud_until = None;
                bars_dirty = true;
            }
            if st.toast_until.is_some_and(|t| now > t) {
                st.toast_until = None;
                drop(st);
                if let Some(ui) = self.ui() {
                    ui.global::<AppState>().set_toast_shown(false);
                }
            }
        }
        if bars_dirty {
            self.refresh_bars();
        }
        self.flush_log();
    }

    fn show_status(&self, st: &Status) {
        let Some(ui) = self.ui() else { return };
        let app = ui.global::<AppState>();
        app.set_mic_level(db_level(st.mic_db));
        app.set_ref_level(db_level(st.ref_db));
        app.set_out_level(db_level(st.out_db));
        app.set_mic_db(s(format!("{:.0} dB", st.mic_db)));
        app.set_ref_db(s(format!("{:.0} dB", st.ref_db)));
        app.set_out_db(s(format!("{:.0} dB", st.out_db)));
        if app.get_page() == 3 && app.get_diag_expanded() {
            self.refresh_diag(Some(st));
        }
    }

    fn refresh_diag(&self, status: Option<&Status>) {
        let Some(ui) = self.ui() else { return };
        let dash = || "—".to_string();
        let siri = self.st.borrow().settings.siri_guard;
        let tips = ui.global::<crate::Tips>();
        let mut rows = vec![
            ("實際使用的引擎", self.engine.engine_name().unwrap_or_else(dash), tips.get_diag_engine()),
            ("喇叭聲音緩衝", status.map_or_else(dash, |s| format!("{:.0} ms", s.ref_buf_ms)), tips.get_diag_buffer()),
            ("時脈漂移", status.map_or_else(dash, |s| format!("{:+.0} ppm", s.drift_ppm)), tips.get_diag_drift()),
            (
                "欠載次數（喇叭／輸出）",
                status.map_or_else(dash, |s| format!("{} ／ {}", s.ref_underruns, s.out_underruns)),
                tips.get_diag_underruns(),
            ),
            ("每 10 ms 音框運算時間", status.map_or_else(dash, |s| format!("{:.2} ms", s.aec_ms)), tips.get_diag_cpu()),
        ];
        if siri {
            rows.push((
                "Hey Siri 處理次數",
                status.map_or_else(dash, |s| s.siri_detections.to_string()),
                tips.get_diag_siri(),
            ));
        }
        let model_rows: Vec<DiagRow> = rows
            .into_iter()
            .map(|(label, value, tip)| DiagRow { label: s(label), value: s(value), tip })
            .collect();
        ui.global::<AppState>().set_diag(model(model_rows));
    }

    // ------------------------------------------------------------ InfoBars

    fn refresh_bars(&self) {
        let Some(ui) = self.ui() else { return };
        let tips = ui.global::<crate::Tips>();
        let st = self.st.borrow();
        let mut home = Vec::new();
        let mut devices = Vec::new();
        let mut siri = Vec::new();

        let no_cable = bar(
            Severity::Error,
            "找不到 VB-Cable",
            "沒有 VB-Cable，對方就聽不到處理過的聲音。請先安裝 VB-Cable，裝好後重新開機。",
            "下載 VB-Cable",
            "download-vbcable",
            &tips.get_vbcable_install(),
        );
        if let Some(err) = &st.pipeline_error {
            home.push(bar(
                Severity::Error,
                "回聲消除停止了",
                &format!("可能是音效裝置被拔掉，或被其他程式佔用。（{}）", short(err)),
                "重新開始",
                "restart",
                "檢查裝置有接好後，按這裡再開始一次。",
            ));
        }
        if let Some(err) = &st.start_error {
            home.push(bar(
                Severity::Error,
                "無法開始",
                &friendly_start_error(err),
                "重新開始",
                "restart",
                "檢查裝置有接好後，按這裡再開始一次。",
            ));
        }
        if st.autostart_failed {
            home.push(bar(
                Severity::Error,
                "開機自動開始失敗",
                "登入 Windows 後找不到需要的音效裝置。請檢查裝置後再開始一次。",
                "重新開始",
                "restart",
                "檢查裝置有接好後，按這裡再開始一次。",
            ));
        }
        if !st.cable.found {
            home.push(no_cable.clone());
            devices.push(no_cable);
        }
        if st.loud_until.is_some() {
            home.push(bar(
                Severity::Warning,
                "麥克風太大聲",
                "請把 Windows 的麥克風音量調低到約 70–80%，不然回音會消不掉。",
                "開啟音效設定",
                "open-sound-settings",
                "開啟 Windows 的音效設定，在「輸入」選你的麥克風，把音量調低。",
            ));
        }
        let missing_ab = !st.cable.has_ab();
        if st.settings.siri_guard && missing_ab {
            home.push(bar(
                Severity::Warning,
                "Hey Siri 防誤觸需要 VB-CABLE A+B",
                "還沒裝第二條虛擬音效線，防誤觸目前無法運作。",
                "下載 VB-CABLE A+B",
                "download-vbcable",
                "在 VB-Audio 網站同一頁下載 VB-CABLE A+B，安裝後重新開機。",
            ));
        }
        if st.settings.siri_guard && st.settings.siri_mode == "detect" && st.settings.siri_model.is_empty() {
            let b = bar(
                Severity::Warning,
                "還沒選模型檔",
                "「偵測到 Hey Siri 才處理」需要喚醒詞模型檔。請選擇模型檔，或改用「一直處理」。",
                "前往設定",
                "goto-siri",
                "到「Hey Siri 防誤觸」頁選擇模型檔。",
            );
            home.push(b.clone());
            siri.push(bar(
                Severity::Warning,
                "還沒選模型檔",
                "「偵測到 Hey Siri 才處理」需要喚醒詞模型檔。請在下方選擇，或改用「一直處理」。",
                "",
                "",
                "",
            ));
        }
        if let Some((ok, msg)) = &st.cable_result {
            devices.push(if *ok {
                bar(
                    Severity::Success,
                    "已整理 VB-Cable",
                    "請重新開啟 Discord / LINE，再選一次麥克風。",
                    "",
                    "",
                    "",
                )
            } else {
                bar(
                    Severity::Error,
                    "整理 VB-Cable 失敗",
                    &format!("Windows 的確認視窗要按「是」才能修改。（{}）", short(msg)),
                    "",
                    "",
                    "",
                )
            });
        }
        if missing_ab {
            siri.push(bar(
                Severity::Warning,
                "需要 VB-CABLE A+B",
                "Hey Siri 防誤觸要用第二條虛擬音效線。在 VB-Audio 網站同一頁下載 VB-CABLE A+B，安裝後重新開機。",
                "下載 VB-CABLE A+B",
                "download-vbcable",
                "在瀏覽器開啟 VB-Audio 的下載頁。",
            ));
        } else {
            let name = self.engine.cable_display_name(&st.devices, "call_speaker");
            siri.push(bar(
                Severity::Info,
                "通話軟體的喇叭要改選",
                &format!("開啟後，Discord / LINE 的喇叭（輸出裝置）要改選「{name}」。本程式沒開時會聽不到對方。"),
                "複製名稱",
                "copy-siri-name",
                &tips.get_copy_siri_name(),
            ));
        }
        let mut advanced = Vec::new();
        if let Some(e) = &st.autostart_error {
            advanced.push(bar(Severity::Error, "無法變更開機自動啟動", &short(e), "", "", ""));
        }
        let calib = st.calib.clone();
        let calibrating = st.calibrating;
        drop(st);

        let app = ui.global::<AppState>();
        app.set_home_bars(model(home));
        app.set_devices_bars(model(devices));
        app.set_siri_bars(model(siri));
        app.set_advanced_bars(model(advanced));
        app.set_calibrating(calibrating);
        app.set_calib_shown(calib.is_some());
        if let Some(c) = calib {
            let (action, id) = if c.suggestion.is_some() {
                ("套用建議值", "apply-calib")
            } else {
                ("", "")
            };
            app.set_calib_bar(bar(c.severity, &c.title, &c.message, action, id, &tips.get_apply_suggestion()));
        }
    }

    fn infobar_action(&self, id: &str) {
        match id {
            "download-vbcable" => self.open_url(VBCABLE_DOWNLOAD_URL),
            "open-sound-settings" => self.open_url(SOUND_SETTINGS_URL),
            "restart" => {
                if self.st.borrow().running {
                    return;
                }
                {
                    let mut st = self.st.borrow_mut();
                    st.pipeline_error = None;
                    st.start_error = None;
                    st.autostart_failed = false;
                }
                self.refresh_devices();
                self.start(StartReason::User);
            }
            "copy-siri-name" => {
                let name = {
                    let st = self.st.borrow();
                    self.engine.cable_display_name(&st.devices, "call_speaker")
                };
                self.copy(&name);
            }
            "goto-siri" => {
                if let Some(ui) = self.ui() {
                    ui.global::<AppState>().set_page(2);
                }
            }
            "apply-calib" => self.apply_calibration(),
            _ => {}
        }
    }

    // ------------------------------------------------------- calibration

    fn calibrate(&self) {
        let (busy, running) = {
            let st = self.st.borrow();
            (st.busy || st.calibrating, st.running)
        };
        if busy {
            return;
        }
        {
            let mut st = self.st.borrow_mut();
            st.calibrating = true;
            st.calib = None;
        }
        self.refresh_bars();
        if running {
            // the pipeline holds the devices: pause it, measure, resume
            self.stop(AfterStop::Calibrate);
        } else {
            self.run_calibration(false);
        }
    }

    fn run_calibration(&self, resume: bool) {
        self.log("量測延遲：喇叭會播放約 4 秒雜訊，請保持安靜…");
        self.set_busy(true, "量測延遲中，喇叭會播放約 4 秒沙沙聲…");
        let settings = self.st.borrow().settings.clone();
        let engine = self.engine.clone();
        let log = self.log_sink();
        self.spawn(move || {
            let r = engine.calibrate(&settings, log).map_err(|e| format!("{e:#}"));
            Msg::Calibrated(r, resume)
        });
    }

    fn on_calibrated(&self, res: Result<(f64, f64), String>, resume: bool) {
        self.set_busy(false, "");
        let engine = self.st.borrow().settings.engine.clone();
        let c = match res {
            Err(e) => {
                self.log(format!("量測失敗：{e}"));
                CalibResult {
                    severity: Severity::Error,
                    title: "量測失敗".into(),
                    message: format!("請確認喇叭和麥克風都有接好。（{}）", short(&e)),
                    suggestion: None,
                }
            }
            Ok((d, conf)) => {
                self.log(format!("量測結果：延遲 {d:.0} ms，信心度 {conf:.1}"));
                if conf < 5.0 {
                    CalibResult {
                        severity: Severity::Warning,
                        title: "量不太準".into(),
                        message: "把喇叭開大聲一點，或讓麥克風靠近喇叭，再量一次。".into(),
                        suggestion: None,
                    }
                } else if d < 0.0 {
                    let v = (-d + 10.0).round();
                    CalibResult {
                        severity: Severity::Info,
                        title: format!("建議把「麥克風延遲」設成 {v:.0} ms"),
                        message: format!("喇叭的聲音比麥克風晚到約 {:.0} ms。", -d),
                        suggestion: Some((0.0, v)),
                    }
                } else if (engine == "speex" || engine == "nlms") && d > 250.0 {
                    let v = (d - 20.0).round();
                    CalibResult {
                        severity: Severity::Info,
                        title: format!("建議把「喇叭延遲」設成 {v:.0} ms"),
                        message: format!("量到的延遲約 {d:.0} ms，比這個引擎能處理的長。"),
                        suggestion: Some((v, 0.0)),
                    }
                } else {
                    CalibResult {
                        severity: Severity::Success,
                        title: "延遲正常，不需要調整".into(),
                        message: format!("量到的延遲約 {d:.0} ms，在正常範圍內。"),
                        suggestion: None,
                    }
                }
            }
        };
        {
            let mut st = self.st.borrow_mut();
            st.calib = Some(c);
            st.calibrating = false;
        }
        self.refresh_bars();
        if resume {
            self.start(StartReason::Resume);
        }
    }

    fn apply_calibration(&self) {
        let Some((d, m)) = self.st.borrow().calib.as_ref().and_then(|c| c.suggestion) else {
            return;
        };
        if let Some(ui) = self.ui() {
            let cfg = ui.global::<Cfg>();
            cfg.set_delay_ms(d as i32);
            cfg.set_mic_delay_ms(m as i32);
        }
        self.st.borrow_mut().calib = Some(CalibResult {
            severity: Severity::Success,
            title: "已套用建議值".into(),
            message: "如果還有殘留回音，可以再量一次。".into(),
            suggestion: None,
        });
        self.read_cfg();
        self.refresh_bars();
    }

    // ------------------------------------------------------------ VB-Cable

    fn cable_actions(&self, actions: Vec<CableAction>) {
        if actions.is_empty() || self.st.borrow().busy {
            return;
        }
        self.st.borrow_mut().cable_result = None;
        if self.st.borrow().running {
            self.stop(AfterStop::Cable(actions));
        } else {
            self.run_cable(actions, false);
        }
    }

    fn run_cable(&self, actions: Vec<CableAction>, resume: bool) {
        self.log("正在整理 VB-Cable 裝置，請在 Windows 確認視窗按「是」…");
        self.set_busy(true, "正在整理 VB-Cable，請在 Windows 的確認視窗按「是」…");
        let engine = self.engine.clone();
        self.spawn(move || Msg::CableDone(engine.cable_apply_elevated(&actions), resume));
    }

    fn on_cable_done(&self, res: (bool, String), resume: bool) {
        self.set_busy(false, "");
        if res.0 {
            self.log(format!("已整理 VB-Cable 裝置：{}", res.1));
            self.log("請重新開啟 Discord / LINE，再選一次麥克風。");
        } else {
            self.log(format!("整理 VB-Cable 裝置失敗：{}", res.1));
        }
        self.st.borrow_mut().cable_result = Some(res);
        self.refresh_devices();
        // Windows can take a moment to publish the new names
        Timer::single_shot(Duration::from_millis(1500), || with_ctrl(|c| c.refresh_devices()));
        if resume {
            self.start(StartReason::Resume);
        }
    }

    // --------------------------------------------------------- misc actions

    fn set_autostart(&self, on: bool) {
        if on {
            self.save_now(); // sign-in start uses the saved settings
        }
        let res = self.engine.set_autostart(on);
        let ok = res.is_ok();
        self.st.borrow_mut().autostart_error = res.err().map(|e| format!("{e:#}"));
        if ok {
            self.log(if on {
                "已設定：登入 Windows 時自動啟動並開始。"
            } else {
                "已取消開機自動啟動。"
            });
        }
        if let Some(ui) = self.ui() {
            ui.global::<AppState>().set_autostart(self.engine.autostart_enabled());
        }
        self.refresh_bars();
    }

    fn copy(&self, text: &str) {
        self.copy_quiet(text, &format!("已複製「{text}」"));
    }

    fn copy_quiet(&self, text: &str, toast: &str) {
        let mut cb = self.clipboard.borrow_mut();
        if cb.is_none() {
            *cb = arboard::Clipboard::new().ok();
        }
        let ok = cb.as_mut().is_some_and(|c| c.set_text(text.to_string()).is_ok());
        drop(cb);
        if ok {
            self.toast(toast);
        } else {
            self.log("無法使用剪貼簿。");
        }
    }

    fn toast(&self, text: &str) {
        self.st.borrow_mut().toast_until = Some(Instant::now() + Duration::from_millis(2200));
        if let Some(ui) = self.ui() {
            let app = ui.global::<AppState>();
            app.set_toast_text(s(text));
            app.set_toast_shown(true);
        }
        // make sure a tick comes to hide it even in slow-poll mode
        Timer::single_shot(Duration::from_millis(2300), || with_ctrl(|c| c.tick()));
    }

    fn open_url(&self, url: &str) {
        if let Err(e) = open::that_detached(url) {
            self.log(format!("無法開啟 {url}：{e}"));
        }
    }

    fn pick_model(&self) {
        if let Some(p) = pick_file() {
            if let Some(ui) = self.ui() {
                ui.global::<Cfg>().set_siri_model(s(p.display().to_string()));
            }
            self.read_cfg();
        }
    }

    fn pick_debug_folder(&self) {
        if let Some(p) = pick_folder() {
            if let Some(ui) = self.ui() {
                ui.global::<Cfg>().set_debug_wav(s(p.display().to_string()));
            }
            self.read_cfg();
        }
    }

    // ---------------------------------------------------- window / tray

    pub fn set_visible(&self, visible: bool) {
        self.st.borrow_mut().visible = visible;
        let running = self.st.borrow().running;
        self.set_poll(running);
        if visible {
            self.st.borrow_mut().log_dirty = true;
            self.flush_log();
        }
    }

    pub fn show_window(&self) {
        let Some(ui) = self.ui() else { return };
        if let Err(e) = ui.show() {
            log::warn!("show window: {e}");
        }
        self.set_visible(true);
        self.refresh_devices();
        super::window_shown(&ui);
    }

    /// X button: hide to the tray.  Returns false when the window must stay.
    fn close_to_tray(&self) -> bool {
        self.save_now();
        let has_tray = self.tray.borrow().is_some();
        if !has_tray {
            // no tray (e.g. Linux): closing quits
            self.quit();
            return true;
        }
        self.set_visible(false);
        let first = !std::mem::replace(&mut self.st.borrow_mut().tray_hint_shown, true);
        if first {
            if let Some(t) = self.tray.borrow().as_ref() {
                t.notify(
                    "Desk Echo Guard 仍在背景執行",
                    "回聲消除會繼續運作。要結束請在右下角圖示按右鍵 →「結束」。",
                );
            }
        }
        true
    }

    pub fn quit(&self) {
        self.save_now();
        *self.tray.borrow_mut() = None;
        if let Some(ui) = self.ui() {
            let _ = ui.hide();
        }
        let (running, busy) = {
            let st = self.st.borrow();
            (st.running, st.busy)
        };
        if running && !busy {
            self.stop(AfterStop::Quit);
            // don't hang on a stuck device
            Timer::single_shot(Duration::from_secs(3), || {
                let _ = slint::quit_event_loop();
            });
        } else {
            let _ = slint::quit_event_loop();
        }
    }

    /// Final cleanup after the event loop ended.
    pub fn shutdown(&self) {
        if self.engine.is_running() {
            self.engine.stop();
        }
        CTRL.with(|c| *c.borrow_mut() = None);
    }
}

fn short(msg: &str) -> String {
    let first = msg.lines().next().unwrap_or("");
    let mut out: String = first.chars().take(120).collect();
    if first.chars().count() > 120 {
        out.push('…');
    }
    out
}

fn friendly_start_error(err: &str) -> String {
    let e = err.to_lowercase();
    if e.contains("cable") {
        "找不到 VB-Cable。請確認已安裝 VB-Cable 並重新開機過。".into()
    } else if e.contains("siri-model") || e.contains("模型") {
        "「偵測到 Hey Siri 才處理」需要模型檔，請到「Hey Siri 防誤觸」頁選擇。".into()
    } else if e.contains("no mic") || e.contains("no speaker") || e.contains("device") {
        format!("找不到選擇的音效裝置，可能被拔掉了。請到「裝置」頁重新選擇。（{}）", short(err))
    } else {
        format!("請確認裝置都有接好，再試一次。（{}）", short(err))
    }
}

#[cfg(windows)]
fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("選擇喚醒詞模型檔")
        .add_filter("喚醒詞模型", &["onnx", "tflite"])
        .add_filter("所有檔案", &["*"])
        .pick_file()
}

#[cfg(windows)]
fn pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().set_title("選擇除錯錄音資料夾").pick_folder()
}

// No file dialog dependency on other platforms (development only).
#[cfg(not(windows))]
fn pick_file() -> Option<PathBuf> {
    std::env::var_os("DEG_PICK_FILE").map(PathBuf::from)
}

#[cfg(not(windows))]
fn pick_folder() -> Option<PathBuf> {
    std::env::var_os("DEG_PICK_FOLDER").map(PathBuf::from)
}
