"""Simple settings window (tkinter, ships with Python on Windows).

    pythonw -m echoguard.gui          (no console window)
    python  -m echoguard.gui --config my.toml

Settings are loaded from / saved to a TOML file (default
%APPDATA%\\desk-echo-guard\\echoguard.toml), the same format ``--config`` reads.
"""

from __future__ import annotations

import argparse
import os
import queue
import sys
import threading
import tkinter as tk
from dataclasses import fields
from tkinter import filedialog, messagebox, ttk

from . import winutil
from .app import EchoGuardApp, Settings, calibrate
from .cli import load_config, save_config, settings_from_dict
from .tray import Tray

DEFAULT = "（系統預設）"
ENGINES = [("auto", "自動（webrtc 優先）"), ("webrtc", "WebRTC AEC3（推薦）"),
           ("speex", "Speex（雙方同時講話較不會吃字）"), ("nlms", "NLMS（內建備援）"),
           ("off", "關閉（只轉送）")]
MODES = [("always", "always：一律陷波（不加延遲）"),
         ("sibilant", "sibilant：偵測 /s/ 音（延遲約 1 秒）"),
         ("detect", "detect：喚醒詞模型（需 .onnx，延遲約 1 秒）")]
PROFILES = [("apple-deep", "apple-deep：仿 Apple 3/4/5/6 kHz、深陷波（推薦）"),
            ("apple", "apple：同上、較淺（可能擋不住）"),
            ("apple-wide", "apple-wide：2.8–6.5 kHz"),
            ("formant", "formant：再削 1–3 kHz（較有效、聲音較悶）")]


def default_config_path() -> str:
    base = os.environ.get("APPDATA") or os.path.join(os.path.expanduser("~"), ".config")
    return os.path.join(base, "desk-echo-guard", "echoguard.toml")


def _labels(pairs):
    return [label for _, label in pairs]


def _key_for(pairs, label, default):
    for k, lab in pairs:
        if lab == label:
            return k
    return default


def _label_for(pairs, key):
    for k, lab in pairs:
        if k == key:
            return lab
    return pairs[0][1]


class EchoGuardGUI:
    def __init__(self, root: tk.Tk, backend=None, config_path: str | None = None,
                 autostart: bool = False, use_tray: bool = True):
        self.root = root
        self.scale = winutil.ui_scale(root)
        self.cmdq: queue.Queue = queue.Queue()  # tray thread -> Tk thread
        self.tray: Tray | None = None
        self._tray_hint_shown = False
        self.backend = backend
        self.config_path = config_path or default_config_path()
        self.app: EchoGuardApp | None = None
        self.busy = False
        self.logq: queue.Queue = queue.Queue()
        self.settings = self._load_settings()

        root.title("Desk Echo Guard")
        self._set_icon()
        root.minsize(self.px(620), self.px(600))
        self._build()
        self._apply_settings_to_widgets(self.settings)
        self.refresh_devices()
        if use_tray:
            self.tray = Tray.create(self._icon_path("icon.png"), self.cmdq.put)
        self.root.protocol("WM_DELETE_WINDOW", self.on_close)
        self.root.after(200, self._poll)
        winutil.refresh_autostart()
        if autostart:  # launched at Windows sign-in: stay in the background
            if self.tray is not None:
                self.root.withdraw()
            else:
                self.root.iconify()
            self.root.after(1000, self._auto_start, 1)

    def px(self, n: float) -> int:
        """Scale a 96-DPI pixel size to the screen (fonts scale on their own)."""
        return int(round(n * self.scale))

    @staticmethod
    def _icon_path(name: str) -> str:
        return os.path.join(os.path.dirname(os.path.abspath(__file__)), "assets", name)

    def _set_icon(self):
        try:
            if sys.platform == "win32":
                ico = self._icon_path("icon.ico")
                self.root.iconbitmap(default=ico)
                winutil.set_window_icon(self.root, ico)  # exact size for this DPI
            else:
                self._icon = tk.PhotoImage(file=self._icon_path("icon.png"))
                self.root.iconphoto(True, self._icon)
        except (tk.TclError, OSError):
            pass  # a missing icon must never stop the program

    # ----------------------------------------------------------- settings io
    def _load_settings(self) -> Settings:
        if os.path.exists(self.config_path):
            try:
                return settings_from_dict(load_config(self.config_path))
            except Exception as e:  # corrupt file: start from defaults
                self.logq.put(f"讀取設定檔失敗，改用預設值：{e}")
        return Settings()

    def collect_settings(self) -> Settings:
        def dev(var):
            v = var.get().strip()
            return None if v in ("", DEFAULT) else v

        def num(var, cast=float, default=0.0):
            try:
                return cast(var.get())
            except (ValueError, tk.TclError):
                return default

        values = dict(self.settings.__dict__)
        values.update(
            mic=dev(self.v_mic), speaker=dev(self.v_speaker),
            out=self.v_out.get().strip() or "CABLE Input",
            call_in=self.v_call_in.get().strip() or "CABLE-A Output",
            engine=_key_for(ENGINES, self.v_engine.get(), "auto"),
            noise_suppression=bool(self.v_ns.get()), high_pass=bool(self.v_hpf.get()),
            agc=bool(self.v_agc.get()),
            delay_ms=num(self.v_delay), mic_delay_ms=num(self.v_mic_delay),
            output_gain_db=num(self.v_gain),
            siri_guard=bool(self.v_siri.get()),
            siri_mode=_key_for(MODES, self.v_mode.get(), "always"),
            siri_profile=_key_for(PROFILES, self.v_profile.get(), "apple"),
            siri_model=[self.v_model.get().strip()] if self.v_model.get().strip() else [],
            siri_threshold=num(self.v_thr, default=0.5),
            debug_wav=self.v_debug.get().strip() or None,
            stats_interval_s=1.0,
        )
        return Settings(**{f.name: values[f.name] for f in fields(Settings)})

    def _apply_settings_to_widgets(self, s: Settings):
        self.v_mic.set(s.mic or DEFAULT)
        self.v_speaker.set(s.speaker or DEFAULT)
        self.v_out.set(s.out)
        self.v_call_in.set(s.call_in)
        self.v_engine.set(_label_for(ENGINES, s.engine))
        self.v_ns.set(s.noise_suppression)
        self.v_hpf.set(s.high_pass)
        self.v_agc.set(s.agc)
        self.v_delay.set(s.delay_ms)
        self.v_mic_delay.set(s.mic_delay_ms)
        self.v_gain.set(s.output_gain_db)
        self.v_siri.set(s.siri_guard)
        self.v_mode.set(_label_for(MODES, s.siri_mode if s.siri_mode in dict(MODES) else "always"))
        self.v_profile.set(_label_for(PROFILES, s.siri_profile))
        self.v_model.set(s.siri_model[0] if s.siri_model else "")
        self.v_thr.set(s.siri_threshold)
        self.v_debug.set(s.debug_wav or "")
        self._update_siri_state()

    def save(self, quiet=False):
        self.settings = self.collect_settings()
        save_config(self.config_path, self.settings)
        if not quiet:
            self.log(f"設定已儲存：{self.config_path}")

    # ---------------------------------------------------------------- layout
    def _build(self):
        px = self.px
        pad = {"padx": px(6), "pady": px(3)}
        frm = ttk.Frame(self.root, padding=px(8))
        frm.pack(fill="both", expand=True)
        frm.columnconfigure(0, weight=1)

        # devices
        dev = ttk.LabelFrame(frm, text="裝置", padding=px(6))
        dev.grid(row=0, column=0, sticky="ew", **pad)
        dev.columnconfigure(1, weight=1)
        self.v_mic, self.v_speaker = tk.StringVar(), tk.StringVar()
        self.v_out, self.v_call_in = tk.StringVar(), tk.StringVar()
        self.cb_mic = self._combo(dev, 0, "實體麥克風", self.v_mic)
        self.cb_speaker = self._combo(dev, 1, "喇叭（回聲參考）", self.v_speaker)
        self.cb_out = self._combo(dev, 2, "虛擬麥克風輸出", self.v_out)
        ttk.Label(dev, text="Discord / LINE 的麥克風請選「CABLE Output」",
                  foreground="#666").grid(row=3, column=1, sticky="w")
        self.btn_refresh = ttk.Button(dev, text="重新整理裝置", command=self.refresh_devices)
        self.btn_refresh.grid(row=0, column=2, rowspan=2, **pad)

        # aec
        aec = ttk.LabelFrame(frm, text="回聲消除", padding=px(6))
        aec.grid(row=1, column=0, sticky="ew", **pad)
        aec.columnconfigure(1, weight=1)
        self.v_engine = tk.StringVar()
        ttk.Label(aec, text="引擎").grid(row=0, column=0, sticky="w", **pad)
        self.cb_engine = ttk.Combobox(aec, textvariable=self.v_engine, values=_labels(ENGINES),
                                      state="readonly")
        self.cb_engine.grid(row=0, column=1, columnspan=3, sticky="ew", **pad)
        self.v_ns, self.v_hpf, self.v_agc = tk.BooleanVar(), tk.BooleanVar(), tk.BooleanVar()
        checks = ttk.Frame(aec)
        checks.grid(row=1, column=1, columnspan=3, sticky="w")
        ttk.Checkbutton(checks, text="降噪", variable=self.v_ns).pack(side="left", padx=4)
        ttk.Checkbutton(checks, text="高通（去低頻嗡聲）", variable=self.v_hpf).pack(side="left", padx=4)
        ttk.Checkbutton(checks, text="自動增益", variable=self.v_agc).pack(side="left", padx=4)
        self.v_delay, self.v_mic_delay, self.v_gain = tk.DoubleVar(), tk.DoubleVar(), tk.DoubleVar()
        nums = ttk.Frame(aec)
        nums.grid(row=2, column=1, columnspan=3, sticky="w")
        for i, (label, var, lo, hi, step) in enumerate((
                ("參考延遲 ms", self.v_delay, 0, 1000, 5),
                ("麥克風延遲 ms", self.v_mic_delay, 0, 500, 5),
                ("輸出增益 dB", self.v_gain, -20, 20, 1))):
            ttk.Label(nums, text=label).grid(row=0, column=2 * i, padx=(4, 2))
            ttk.Spinbox(nums, textvariable=var, from_=lo, to=hi, increment=step,
                        width=7).grid(row=0, column=2 * i + 1, padx=(0, 10))
        ttk.Label(aec, text="延遲通常不用設；不確定時按「量測延遲」",
                  foreground="#666").grid(row=3, column=1, columnspan=3, sticky="w")

        # siri
        siri = ttk.LabelFrame(frm, text="Hey Siri 防誤觸", padding=px(6))
        siri.grid(row=2, column=0, sticky="ew", **pad)
        siri.columnconfigure(1, weight=1)
        self.v_siri = tk.BooleanVar()
        ttk.Checkbutton(siri, text="啟用（Discord / LINE 的輸出裝置要改成 CABLE-A Input）",
                        variable=self.v_siri, command=self._update_siri_state
                        ).grid(row=0, column=0, columnspan=3, sticky="w", **pad)
        self.cb_call = self._combo(siri, 1, "通話聲音來源", self.v_call_in)
        self.v_mode, self.v_profile = tk.StringVar(), tk.StringVar()
        ttk.Label(siri, text="模式").grid(row=2, column=0, sticky="w", **pad)
        self.cb_mode = ttk.Combobox(siri, textvariable=self.v_mode, values=_labels(MODES),
                                    state="readonly")
        self.cb_mode.grid(row=2, column=1, columnspan=2, sticky="ew", **pad)
        self.cb_mode.bind("<<ComboboxSelected>>", lambda e: self._update_siri_state())
        ttk.Label(siri, text="陷波強度").grid(row=3, column=0, sticky="w", **pad)
        self.cb_profile = ttk.Combobox(siri, textvariable=self.v_profile,
                                       values=_labels(PROFILES), state="readonly")
        self.cb_profile.grid(row=3, column=1, columnspan=2, sticky="ew", **pad)
        self.v_model, self.v_thr = tk.StringVar(), tk.DoubleVar()
        ttk.Label(siri, text="模型檔").grid(row=4, column=0, sticky="w", **pad)
        self.en_model = ttk.Entry(siri, textvariable=self.v_model)
        self.en_model.grid(row=4, column=1, sticky="ew", **pad)
        self.btn_model = ttk.Button(siri, text="瀏覽…", command=self._pick_model)
        self.btn_model.grid(row=4, column=2, **pad)
        ttk.Label(siri, text="偵測門檻").grid(row=5, column=0, sticky="w", **pad)
        self.sp_thr = ttk.Spinbox(siri, textvariable=self.v_thr, from_=0.05, to=0.99,
                                  increment=0.05, width=7)
        self.sp_thr.grid(row=5, column=1, sticky="w", **pad)
        self.siri_widgets = [self.cb_call, self.cb_mode, self.cb_profile]
        self.detect_widgets = [self.en_model, self.btn_model, self.sp_thr]

        # buttons
        bar = ttk.Frame(frm)
        bar.grid(row=3, column=0, sticky="ew", **pad)
        self.btn_start = ttk.Button(bar, text="▶ 開始", command=self.toggle)
        self.btn_start.pack(side="left", padx=4)
        self.btn_cal = ttk.Button(bar, text="量測延遲", command=self.calibrate)
        self.btn_cal.pack(side="left", padx=4)
        ttk.Button(bar, text="儲存設定", command=self.save).pack(side="left", padx=4)
        self.v_debug = tk.StringVar()
        ttk.Label(bar, text="除錯錄音資料夾").pack(side="left", padx=(16, 2))
        ttk.Entry(bar, textvariable=self.v_debug, width=14).pack(side="left")
        self.v_autostart = tk.BooleanVar(value=winutil.get_autostart() is not None)
        self.chk_autostart = ttk.Checkbutton(
            frm, text="登入 Windows 時自動開啟並開始（使用已儲存的設定）",
            variable=self.v_autostart, command=self._toggle_autostart)
        self.chk_autostart.grid(row=5, column=0, sticky="w", **pad)
        if not winutil.IS_WIN:
            self.chk_autostart.configure(state="disabled")

        # status
        st = ttk.LabelFrame(frm, text="狀態", padding=px(6))
        st.grid(row=4, column=0, sticky="nsew", **pad)
        frm.rowconfigure(4, weight=1)
        self.v_status = tk.StringVar(value="已停止")
        ttk.Label(st, textvariable=self.v_status, font=("TkDefaultFont", 10, "bold")
                  ).pack(anchor="w")
        meters = ttk.Frame(st)
        meters.pack(fill="x", pady=4)
        self.meters = {}
        for i, (k, label) in enumerate((("mic", "麥克風"), ("ref", "喇叭"), ("out", "輸出"))):
            ttk.Label(meters, text=label, width=6).grid(row=i, column=0, sticky="w")
            bar_ = ttk.Progressbar(meters, maximum=60, length=px(300))
            bar_.grid(row=i, column=1, sticky="ew", padx=4)
            val = ttk.Label(meters, text="", width=9)
            val.grid(row=i, column=2)
            self.meters[k] = (bar_, val)
        meters.columnconfigure(1, weight=1)
        self.txt = tk.Text(st, height=8, wrap="word", state="disabled")
        self.txt.pack(fill="both", expand=True)

    def _combo(self, parent, row, label, var):
        pad = {"padx": self.px(6), "pady": self.px(3)}
        ttk.Label(parent, text=label).grid(row=row, column=0, sticky="w", **pad)
        cb = ttk.Combobox(parent, textvariable=var)
        cb.grid(row=row, column=1, sticky="ew", **pad)
        return cb

    def _update_siri_state(self):
        on = bool(self.v_siri.get()) and self.app is None
        for w in self.siri_widgets:
            w.configure(state=("readonly" if isinstance(w, ttk.Combobox) and w is not self.cb_call
                               else "normal") if on else "disabled")
        det = on and _key_for(MODES, self.v_mode.get(), "always") == "detect"
        for w in self.detect_widgets:
            w.configure(state="normal" if det else "disabled")

    def _pick_model(self):
        p = filedialog.askopenfilename(title="選擇 openWakeWord 模型",
                                       filetypes=[("Wake-word model", "*.onnx *.tflite"),
                                                  ("All", "*.*")])
        if p:
            self.v_model.set(p)

    # --------------------------------------------------------------- actions
    def _get_backend(self):
        if self.backend is None:
            from .io_backend import SoundcardBackend
            self.backend = SoundcardBackend()
        return self.backend

    def refresh_devices(self):
        try:
            devs = self._get_backend().list_devices()
        except Exception as e:
            self.log(f"無法列出裝置：{e}")
            return
        mics = [d.name for d in devs if d.kind == "mic"]
        spks = [d.name for d in devs if d.kind == "speaker"]
        self.cb_mic["values"] = [DEFAULT] + mics
        self.cb_speaker["values"] = [DEFAULT] + spks
        self.cb_out["values"] = spks
        self.cb_call["values"] = mics
        if not any("cable" in n.lower() for n in spks):
            self.log("找不到 VB-Cable（CABLE Input）。請先安裝 VB-CABLE 驅動並重新開機。")

    def log(self, msg: str):
        self.logq.put(str(msg))

    def _write_log(self, msg):
        self.txt.configure(state="normal")
        self.txt.insert("end", msg + "\n")
        lines = int(self.txt.index("end-1c").split(".")[0])
        if lines > 300:
            self.txt.delete("1.0", f"{lines - 300}.0")
        self.txt.see("end")
        self.txt.configure(state="disabled")

    def _set_running_ui(self, running: bool):
        """Settings only apply on start, so lock every input while running."""
        readonly = {self.cb_engine, self.cb_mode, self.cb_profile}

        def walk(w):
            for c in w.winfo_children():
                if isinstance(c, (ttk.Combobox, ttk.Entry, ttk.Spinbox, ttk.Checkbutton,
                                  ttk.Button)) and c not in (self.btn_start,
                                                             self.chk_autostart):
                    if running:
                        c.configure(state="disabled")
                    else:
                        c.configure(state="readonly" if c in readonly else "normal")
                walk(c)
        walk(self.root)
        self.btn_start.configure(text="■ 停止" if running else "▶ 開始")
        if self.tray is not None:
            self.tray.set_running(running)
        if not running:
            self._update_siri_state()

    def toggle(self):
        if self.busy:
            return
        if self.app is None:
            self.start()
        else:
            self.stop()

    def start(self, interactive: bool = True) -> bool:
        st = self.collect_settings()
        if st.siri_guard and st.siri_mode == "detect" and not st.siri_model:
            msg = "detect 模式需要選擇模型檔，或改用 always / sibilant 模式。"
            if interactive:
                messagebox.showwarning("Desk Echo Guard", msg)
            else:
                self.log(msg)
            return False
        self.save(quiet=True)
        app = EchoGuardApp(st, self._get_backend(), log=self.log)
        try:
            app.start()
        except Exception as e:
            try:
                app.stop()
            except Exception:
                pass
            if interactive:
                messagebox.showerror("Desk Echo Guard", f"無法啟動：\n{e}")
            else:
                self.log(f"無法啟動：{e}")
            return False
        self.app = app
        self._set_running_ui(True)
        self.v_status.set("執行中 — 對方應該聽不到回音了")
        self.log("已開始。")
        return True

    AUTO_START_TRIES = 12  # audio devices (VB-Cable) can appear late after sign-in

    def _auto_start(self, attempt: int):
        if self.app is not None or self.busy:
            return
        if attempt > 1:
            self.refresh_devices()
        if self.start(interactive=False):
            return
        if attempt < self.AUTO_START_TRIES:
            self.log(f"自動開始失敗，5 秒後重試（{attempt}/{self.AUTO_START_TRIES}）")
            self.root.after(5000, self._auto_start, attempt + 1)
        else:
            self.show_window()
            self.v_status.set("自動開始失敗，請檢查裝置後按「▶ 開始」")

    def _toggle_autostart(self):
        want = bool(self.v_autostart.get())
        try:
            if want:
                self.save(quiet=True)  # sign-in start uses the saved settings
            winutil.set_autostart(want)
        except OSError as e:
            self.v_autostart.set(not want)
            messagebox.showerror("Desk Echo Guard", f"無法變更開機自動啟動：\n{e}")
            return
        self.log("已設定：登入 Windows 時自動開啟並開始。" if want else "已取消開機自動啟動。")

    def stop(self, then=None):
        app, self.app = self.app, None
        if app is None:
            if then:
                then()
            return
        self.busy = True
        self.v_status.set("停止中…")

        def work():
            try:
                app.stop()
            finally:
                self.logq.put(("stopped", then))
        threading.Thread(target=work, daemon=True).start()

    def calibrate(self):
        if self.app is not None or self.busy:
            return
        st = self.collect_settings()
        self.busy = True
        self.btn_cal.configure(state="disabled")
        self.v_status.set("量測中：喇叭會播放約 4 秒雜訊…")

        def work():
            try:
                d, conf = calibrate(st, self._get_backend(), log=self.log)
                if conf < 5:
                    self.log("信心度偏低：把喇叭開大聲一點或讓麥克風靠近喇叭後重試。")
                elif d < 0:
                    self.log(f"參考訊號晚到：建議把「麥克風延遲」設成 {-d + 10:.0f} ms。")
                elif st.engine in ("speex", "nlms") and d > 250:
                    self.log(f"延遲較長：建議把「參考延遲」設成 {d - 20:.0f} ms。")
                else:
                    self.log("延遲在正常範圍，不需要調整。")
            except Exception as e:
                self.log(f"量測失敗：{e}")
            finally:
                self.logq.put(("calibrated", None))
        threading.Thread(target=work, daemon=True).start()

    def on_close(self):
        """The window's X hides to the tray; quit from the tray menu."""
        if self.tray is None:
            self.quit()
            return
        try:
            self.save(quiet=True)
        except Exception:
            pass
        self.root.withdraw()
        if not self._tray_hint_shown:
            self._tray_hint_shown = True
            self.tray.notify("Desk Echo Guard 仍在背景執行。要結束請在右下角圖示按右鍵 →「結束」。")

    def show_window(self):
        self.root.deiconify()
        self.root.lift()
        self.root.focus_force()

    def quit(self):
        if self.tray is not None:
            self.tray.stop()
            self.tray = None
        try:
            self.save(quiet=True)
        except Exception:
            pass
        self.stop(then=self.root.destroy)
        if self.busy:
            self.root.after(3000, self.root.destroy)  # don't hang on a stuck device

    # ----------------------------------------------------------------- poll
    def _poll(self):
        try:
            while True:
                cmd = self.cmdq.get_nowait()
                if cmd == "show":
                    self.show_window()
                elif cmd == "toggle":
                    if self.app is None:
                        if not self.start(interactive=False):
                            self.show_window()  # let the user see why
                    else:
                        self.stop()
                elif cmd == "quit":
                    self.quit()
                    if self.app is None and not self.busy:
                        return  # window already destroyed
        except queue.Empty:
            pass
        try:
            while True:
                item = self.logq.get_nowait()
                if isinstance(item, tuple):
                    kind, then = item
                    self.busy = False
                    if kind == "stopped":
                        self._set_running_ui(False)
                        self.v_status.set("已停止")
                        self._reset_meters()
                        if then:
                            then()
                            return
                    else:
                        self.btn_cal.configure(state="normal")
                        self.v_status.set("已停止")
                else:
                    self._write_log(item)
        except queue.Empty:
            pass
        app = self.app
        if app is not None:
            if app.errors:
                err = app.errors[0]
                self.log(f"錯誤：{err}")
                self.stop()
                messagebox.showerror("Desk Echo Guard", f"執行中發生錯誤，已停止：\n{err}")
            else:
                self._update_meters(app)
        self.root.after(200, self._poll)

    def _update_meters(self, app):
        for k, (bar, lab) in self.meters.items():
            v = app.levels.get(k, -120.0)
            bar["value"] = max(0.0, min(60.0, v + 60.0))
            lab.configure(text=f"{v:6.1f} dB")
        r = app.ref_buf.stats()
        extra = f"參考緩衝 {r['level'] / 48:.0f} ms，漂移 {r['drift_ppm']:+.0f} ppm"
        if app.guard is not None:
            extra += f"，Siri 處理 {app.guard.detections} 次"
        if app.mic_peak > 0.9:
            extra += "　⚠ 麥克風太大聲，請調低 Windows 麥克風音量"
            app.mic_peak = 0.0
        self.v_status.set("執行中 — " + extra)

    def _reset_meters(self):
        for bar, lab in self.meters.values():
            bar["value"] = 0
            lab.configure(text="")


def main(argv=None, backend=None):
    ap = argparse.ArgumentParser(prog="echoguard-gui")
    ap.add_argument("--config", help=f"settings file (default {default_config_path()})")
    ap.add_argument(winutil.AUTOSTART_FLAG, action="store_true",
                    help="start in the tray and begin processing (used at Windows sign-in)")
    args = ap.parse_args(argv)
    winutil.enable_dpi_awareness()  # before Tk() so fonts and sizes follow the DPI
    if backend is None and not winutil.claim_single_instance():
        if not args.autostart:
            root = tk.Tk()
            root.withdraw()
            messagebox.showinfo("Desk Echo Guard", "Desk Echo Guard 已經在執行中。\n"
                                "請從右下角系統匣的圖示開啟視窗。")
            root.destroy()
        return 0
    root = tk.Tk()
    gui = EchoGuardGUI(root, backend=backend, config_path=args.config, autostart=args.autostart)
    try:
        root.mainloop()
    finally:  # the tray thread would otherwise keep the process alive
        if getattr(gui, "tray", None) is not None:
            gui.tray.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
