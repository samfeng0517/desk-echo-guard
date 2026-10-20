"""GUI smoke test against simulated devices (needs tkinter + a display;
run under xvfb-run on Linux CI, skipped otherwise)."""

import os
import time

import numpy as np
import pytest

tk = pytest.importorskip("tkinter")

from echoguard import vcable  # noqa: E402
from echoguard.signals import make_scenario, synth_speech  # noqa: E402
from echoguard.sim import SimBackend  # noqa: E402


@pytest.fixture
def root():
    try:
        r = tk.Tk()
    except tk.TclError:
        pytest.skip("no display")
    r.withdraw()
    yield r
    try:
        r.destroy()
    except tk.TclError:
        pass


def _pump(root, seconds):
    end = time.time() + seconds
    while time.time() < end:
        root.update()
        time.sleep(0.02)


def test_gui_start_stop_and_config_roundtrip(root, tmp_path):
    from echoguard.gui import DEFAULT, EchoGuardGUI
    sc = make_scenario(dur_s=10, delay_ms=40)
    be = SimBackend(inputs={"USB Mic": (sc.mic, 0.0), "CABLE-A Output": (synth_speech(10), 0.0)},
                    loopbacks={"Speakers": (sc.ref, 0.0)},
                    outputs={"CABLE Input": 0.0, "Speakers": 0.0})
    cfg = tmp_path / "gui.toml"
    gui = EchoGuardGUI(root, backend=be, config_path=str(cfg), use_tray=False)
    assert "USB Mic" in gui.cb_mic["values"] and DEFAULT in gui.cb_mic["values"]
    assert getattr(gui, "_icon", None) is not None or os.name == "nt"  # icon loaded

    gui.v_mic.set("USB Mic")
    gui.v_speaker.set("Speakers")
    gui.v_out.set("CABLE Input")
    gui.v_engine.set("NLMS（內建備援）")
    gui.v_siri.set(True)
    gui._update_siri_state()
    gui.v_call_in.set("CABLE-A Output")
    gui.v_mode.set(next(lab for k, lab in __import__("echoguard.gui", fromlist=["MODES"]).MODES
                        if k == "always"))
    gui.start()
    assert gui.app is not None, "start failed"
    _pump(root, 1.5)
    assert gui.app.frames > 50
    assert str(gui.cb_mic["state"]) == "disabled" and str(gui.cb_engine["state"]) == "disabled"
    assert gui.meters["mic"][0]["value"] > 0
    gui.toggle()  # stop
    _pump(root, 1.0)
    assert gui.app is None and not gui.busy
    assert str(gui.cb_engine["state"]) == "readonly" and str(gui.cb_mic["state"]) == "normal"
    assert len(be.opened_outputs["CABLE Input"].data()) > 0
    assert len(be.opened_outputs["Speakers"].data()) > 0  # siri guard path ran

    # settings were saved on start and load back into a fresh window
    assert cfg.exists()
    gui2 = EchoGuardGUI(root, backend=be, config_path=str(cfg), use_tray=False)
    s = gui2.collect_settings()
    assert s.mic == "USB Mic" and s.engine == "nlms" and s.siri_guard is True
    assert s.siri_mode == "always"


class _FakeTray:
    def __init__(self):
        self.notes, self.stopped, self.running = [], False, None

    def notify(self, msg):
        self.notes.append(msg)

    def set_running(self, running):
        self.running = running

    def stop(self):
        self.stopped = True


def test_close_hides_to_tray_and_quit_from_tray(root, tmp_path):
    from echoguard.gui import EchoGuardGUI
    be = SimBackend(inputs={}, loopbacks={}, outputs={"CABLE Input": 0.0})
    gui = EchoGuardGUI(root, backend=be, config_path=str(tmp_path / "g.toml"), use_tray=False)
    gui.tray = tray = _FakeTray()
    root.deiconify()
    gui.on_close()
    _pump(root, 0.2)
    assert root.state() == "withdrawn" and len(tray.notes) == 1
    gui.cmdq.put("show")
    _pump(root, 0.4)
    assert root.state() == "normal"
    gui.cmdq.put("quit")
    _pump(root, 0.4)
    assert tray.stopped
    with pytest.raises(tk.TclError):
        root.winfo_exists()


def test_autostart_command_points_at_this_install():
    from echoguard import winutil
    cmd = winutil.autostart_command()
    assert winutil.AUTOSTART_FLAG in cmd and "echoguard.gui" in cmd


def test_autostart_flag_parsed(monkeypatch):
    from echoguard import gui as gui_mod
    seen = {}

    class Stub:
        def __init__(self, root, backend=None, config_path=None, autostart=False):
            seen["autostart"] = autostart
            root.destroy()

    monkeypatch.setattr(gui_mod, "EchoGuardGUI", Stub)
    monkeypatch.setattr(gui_mod.tk.Tk, "mainloop", lambda self: None)
    # a real DeskEchoGuard running on this machine must not make main() bail out
    monkeypatch.setattr(gui_mod.winutil, "claim_single_instance", lambda: True)
    try:
        gui_mod.main(["--autostart"])
    except tk.TclError:
        pytest.skip("no display")
    assert seen["autostart"] is True


def test_cable_status_follows_device_names(root, tmp_path):
    from echoguard.gui import EchoGuardGUI
    be = SimBackend(inputs={"USB Mic": (np.zeros(480, np.float32), 0.0)}, loopbacks={},
                    outputs={"Speakers": 0.0})
    gui = EchoGuardGUI(root, backend=be, config_path=str(tmp_path / "g.toml"), use_tray=False)
    assert "找不到" in gui.v_cable.get() and gui.btn_cable["text"] == "下載 VB-Cable"
    assert not gui.adv.winfo_ismapped() and not gui.v_adv.get()  # advanced folded away

    be.inputs["CABLE Output (VB-Audio Virtual Cable)"] = (np.zeros(480, np.float32), 0.0)
    be.output_ppm["CABLE Input (VB-Audio Virtual Cable)"] = 0.0
    be.output_ppm["CABLE-A Input (VB-Audio Cable A)"] = 0.0
    gui.refresh_devices()
    assert "「CABLE Output」" in gui.v_cable.get() and gui.btn_cable.winfo_manager()
    assert "「CABLE-A Input」" in gui.chk_siri["text"]

    for kind, d in (("mic", be.inputs), ("speaker", be.output_ppm)):
        for name in list(d):
            if (r := vcable.role_of(name, kind)):
                d[name.replace(r.original, r.renamed, 1)] = d.pop(name)
    gui.refresh_devices()
    assert "「回聲消除麥克風」" in gui.v_cable.get()
    assert not gui.btn_cable.winfo_manager()  # nothing left to rename
    assert "「Siri防護喇叭」" in gui.chk_siri["text"]

    be.output_ppm["CABLE In 16ch (VB-Audio Virtual Cable)"] = 0.0
    gui.refresh_devices()
    assert gui.btn_cable.winfo_manager() and gui.btn_cable["text"] == "停用用不到的裝置…"
    assert "1 個用不到" in gui.v_cable.get()
