"""GUI smoke test against simulated devices (needs tkinter + a display;
run under xvfb-run on Linux CI, skipped otherwise)."""

import os
import time

import numpy as np
import pytest

tk = pytest.importorskip("tkinter")

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
    gui = EchoGuardGUI(root, backend=be, config_path=str(cfg))
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
    gui2 = EchoGuardGUI(root, backend=be, config_path=str(cfg))
    s = gui2.collect_settings()
    assert s.mic == "USB Mic" and s.engine == "nlms" and s.siri_guard is True
    assert s.siri_mode == "always"
