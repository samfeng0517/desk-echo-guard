//! System tray icon (`tray-icon`, Windows): 顯示視窗 / 開始・停止 / 結束,
//! double-click shows the window, and a balloon notification the first
//! time the window is closed to the tray.  Menu events arrive on the UI
//! thread's message loop (winit) and are forwarded with `on_cmd`.
//!
//! On other platforms there is no tray (`Tray::new` returns `Ok(None)`),
//! so closing the window quits.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))] // only the Windows tray sends these
pub enum TrayCmd {
    Show,
    Toggle,
    Quit,
}

/// Decode the embedded PNG icon to (rgba, width, height).
#[allow(dead_code)]
pub fn icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    static PNG: &[u8] = include_bytes!("../../../../echoguard/assets/icon.png");
    let mut dec = png::Decoder::new(std::io::Cursor::new(PNG));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let px = (info.width * info.height) as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..px * 4].to_vec(),
        png::ColorType::Rgb => buf[..px * 3]
            .chunks(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf[..px * 2]
            .chunks(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None, // EXPAND turns this into RGB(A)
    };
    Some((rgba, info.width, info.height))
}

#[cfg(windows)]
mod imp {
    use super::{icon_rgba, TrayCmd};
    use anyhow::{Context, Result};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct Tray {
        icon: TrayIcon,
        toggle: MenuItem,
    }

    impl Tray {
        pub fn new(
            on_cmd: impl Fn(TrayCmd) + Send + Sync + Clone + 'static,
        ) -> Result<Option<Self>> {
            let show = MenuItem::new("顯示視窗", true, None);
            let toggle = MenuItem::new("開始", true, None);
            let quit = MenuItem::new("結束", true, None);
            let menu = Menu::new();
            menu.append_items(&[&show, &toggle, &PredefinedMenuItem::separator(), &quit])?;
            let (rgba, w, h) = icon_rgba().context("decode tray icon")?;
            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Desk Echo Guard（已停止）")
                .with_icon(Icon::from_rgba(rgba, w, h)?)
                .build()?;

            let (show_id, toggle_id, quit_id) =
                (show.id().clone(), toggle.id().clone(), quit.id().clone());
            let cb = on_cmd.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if e.id == show_id {
                    cb(TrayCmd::Show);
                } else if e.id == toggle_id {
                    cb(TrayCmd::Toggle);
                } else if e.id == quit_id {
                    cb(TrayCmd::Quit);
                }
            }));
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } = e
                {
                    on_cmd(TrayCmd::Show);
                }
            }));
            Ok(Some(Tray { icon, toggle }))
        }

        pub fn set_running(&self, running: bool) {
            self.toggle.set_text(if running { "停止" } else { "開始" });
            let tip = if running {
                "Desk Echo Guard（運作中）"
            } else {
                "Desk Echo Guard（已停止）"
            };
            let _ = self.icon.set_tooltip(Some(tip));
        }

        /// Balloon / toast from the tray icon (Shell_NotifyIcon NIF_INFO).
        pub fn notify(&self, title: &str, message: &str) {
            use windows_sys::Win32::UI::Shell::{
                Shell_NotifyIconW, NIF_INFO, NIIF_INFO, NIM_MODIFY, NOTIFYICONDATAW,
            };
            fn copy_wide(dst: &mut [u16], s: &str) {
                let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
                dst[..w.len()].copy_from_slice(&w);
                dst[w.len()] = 0;
            }
            // SAFETY: plain FFI struct, zero is a valid initial state.
            let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = self.icon.window_handle() as _;
            // tray-icon numbers its icons from 1; this is the only one
            data.uID = 1;
            data.uFlags = NIF_INFO;
            data.dwInfoFlags = NIIF_INFO;
            copy_wide(&mut data.szInfoTitle, title);
            copy_wide(&mut data.szInfo, message);
            // SAFETY: data is fully initialised and outlives the call.
            unsafe {
                Shell_NotifyIconW(NIM_MODIFY, &data);
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            MenuEvent::set_event_handler::<fn(MenuEvent)>(None);
            TrayIconEvent::set_event_handler::<fn(TrayIconEvent)>(None);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::TrayCmd;

    pub struct Tray;

    impl Tray {
        pub fn new(
            _on_cmd: impl Fn(TrayCmd) + Send + Sync + Clone + 'static,
        ) -> anyhow::Result<Option<Self>> {
            Ok(None)
        }
        pub fn set_running(&self, _running: bool) {}
        pub fn notify(&self, _title: &str, _message: &str) {}
    }
}

pub use imp::Tray;
