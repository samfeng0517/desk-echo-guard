import numpy as np
from scipy.io import wavfile

from echoguard.cli import build_parser, main, make_settings
from echoguard.metrics import erle_db
from echoguard.signals import make_scenario


def test_config_file_and_cli_override(tmp_path):
    cfg = tmp_path / "c.toml"
    cfg.write_text('mic = "USB"\ndelay-ms = 40\n[siri]\nsiri_mode = "always"\n'
                   'siri_model = "x.onnx"\n', encoding="utf-8")
    args = build_parser().parse_args(["--config", str(cfg), "--delay-ms", "12"])
    st = make_settings(args)
    assert st.mic == "USB" and st.delay_ms == 12.0 and st.siri_mode == "always"
    assert st.siri_model == ["x.onnx"] and st.out == "CABLE Input"


def _w(path, x):
    wavfile.write(path, 48000, np.clip(x * 32767, -32768, 32767).astype(np.int16))


def test_offline_sim_commands(tmp_path):
    sc = make_scenario(dur_s=12.0, delay_ms=40)
    _w(tmp_path / "mic.wav", sc.mic)
    _w(tmp_path / "ref.wav", sc.ref)
    assert main(["--engine", "nlms", "--sim-mic", str(tmp_path / "mic.wav"),
                 "--sim-ref", str(tmp_path / "ref.wav"), "--sim-out", str(tmp_path / "o.wav")]) == 0
    _, o = wavfile.read(tmp_path / "o.wav")
    o = o / 32768.0
    assert erle_db(sc.mic[:len(o)], o, sc.far_only[:len(o)], skip_s=3) > 8
    assert main(["--siri-mode", "always", "--sim-guard-in", str(tmp_path / "ref.wav"),
                 "--sim-guard-out", str(tmp_path / "g.wav")]) == 0
    assert (tmp_path / "g.wav").exists()
