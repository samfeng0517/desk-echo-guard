"""Command line interface.

    python -m echoguard --list-devices
    python -m echoguard --mic "USB" --speaker "Realtek" --out "CABLE Input"
    python -m echoguard --config echoguard.toml --siri-guard --siri-mode always
    python -m echoguard --calibrate --mic "USB" --speaker "Realtek"
    python -m echoguard --sim-mic mic.wav --sim-ref ref.wav --sim-out out.wav
    python -m echoguard --sim-guard-in call.wav --sim-guard-out call_guarded.wav --siri-mode always
"""

from __future__ import annotations

import argparse
import sys
from dataclasses import fields

from . import FS, __version__
from .app import Settings


def _bool(v):
    if isinstance(v, bool):
        return v
    return str(v).lower() in ("1", "true", "yes", "on")


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="echoguard",
        description="System-wide echo cancellation (speaker loopback reference -> "
                    "VB-Cable virtual mic) + optional 'Hey Siri' anti-trigger filter.")
    S = argparse.SUPPRESS
    p.add_argument("--version", action="version", version=__version__)
    p.add_argument("--config", help="TOML config file (keys = long option names, "
                                    "with - or _)")
    p.add_argument("--list-devices", action="store_true", help="list audio devices and exit")
    p.add_argument("--list-engines", action="store_true", help="list available AEC engines")
    p.add_argument("--rename-vbcable", choices=["rename", "restore"],
                   help="give the VB-Cable devices descriptive names (or restore VB-Audio's); "
                        "run from an administrator prompt")
    p.add_argument("--calibrate", action="store_true",
                   help="play noise bursts, measure speaker->mic delay, print a --delay-ms hint")

    g = p.add_argument_group("devices (case-insensitive name substring)")
    g.add_argument("--mic", default=S, help="physical microphone (default: system default)")
    g.add_argument("--speaker", default=S,
                   help="real speakers: loopback reference and Siri-guard output "
                        "(default: system default output)")
    g.add_argument("--out", default=S, help='virtual mic sink (default "CABLE Input")')
    g.add_argument("--call-in", default=S,
                   help='--siri-guard: where Discord/LINE plays to (default "CABLE-A Output")')

    a = p.add_argument_group("echo cancellation")
    a.add_argument("--engine", default=S, choices=["auto", "webrtc", "speex", "nlms", "off"])
    a.add_argument("--delay-ms", type=float, default=S,
                   help="extra delay applied to the reference (ms); see --calibrate")
    a.add_argument("--mic-delay-ms", type=float, default=S,
                   help="extra delay applied to the mic, if the reference arrives late")
    a.add_argument("--noise-suppression", type=_bool, default=S, metavar="on|off")
    a.add_argument("--high-pass", type=_bool, default=S, metavar="on|off")
    a.add_argument("--agc", type=_bool, default=S, metavar="on|off")
    a.add_argument("--filter-ms", type=int, default=S, help="speex/nlms filter length")
    a.add_argument("--output-gain-db", type=float, default=S)
    a.add_argument("--ref-buffer-ms", type=float, default=S)
    a.add_argument("--out-buffer-ms", type=float, default=S)
    a.add_argument("--max-ppm", type=float, default=S, help="max clock-drift correction")
    a.add_argument("--blocksize", type=int, default=S, help="device block size (samples)")

    s = p.add_argument_group("Hey Siri anti-trigger")
    s.add_argument("--siri-guard", action="store_const", const=True, default=S,
                   help="enable pipeline B (call audio -> filter -> speakers)")
    s.add_argument("--siri-mode", default=S, choices=["detect", "sibilant", "always"])
    s.add_argument("--siri-model", action="append", default=S,
                   help="openWakeWord model (.onnx/.tflite) for --siri-mode detect")
    s.add_argument("--siri-threshold", type=float, default=S)
    s.add_argument("--siri-profile", default=S,
                   choices=["apple", "apple-deep", "apple-wide", "formant"])
    s.add_argument("--siri-depth-db", type=float, default=S)
    s.add_argument("--siri-lookahead-s", type=float, default=S)

    d = p.add_argument_group("debug / offline")
    d.add_argument("--debug-wav", default=S, help="directory to dump mic/ref/out wavs on exit")
    d.add_argument("--stats-interval-s", type=float, default=S)
    d.add_argument("--duration-s", type=float, default=None, help="stop after N seconds")
    d.add_argument("--sim-mic", help="offline AEC: mic recording (wav)")
    d.add_argument("--sim-ref", help="offline AEC: reference recording (wav)")
    d.add_argument("--sim-out", help="offline AEC: output wav")
    d.add_argument("--sim-guard-in", help="offline Siri guard: input wav")
    d.add_argument("--sim-guard-out", help="offline Siri guard: output wav")
    return p


def load_config(path: str) -> dict:
    try:
        import tomllib
    except ModuleNotFoundError:  # Python < 3.11
        import tomli as tomllib  # type: ignore
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    flat = {}
    for k, v in raw.items():
        if isinstance(v, dict):  # allow [aec] / [siri] / [devices] tables
            for k2, v2 in v.items():
                flat[k2.replace("-", "_")] = v2
        else:
            flat[k.replace("-", "_")] = v
    return flat


def _toml_value(v) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(v)
    if isinstance(v, (list, tuple)):
        return "[" + ", ".join(_toml_value(x) for x in v) + "]"
    s = str(v).replace("\\", "\\\\").replace('"', '\\"')
    return f'"{s}"'


def save_config(path: str, settings: Settings) -> None:
    """Write every non-None setting as a flat TOML file (readable by
    load_config / --config)."""
    import os
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    lines = ["# desk-echo-guard settings (written by the GUI; --config reads it too)"]
    for f in fields(Settings):
        v = getattr(settings, f.name)
        if v is not None:
            lines.append(f"{f.name} = {_toml_value(v)}")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")


def settings_from_dict(values: dict) -> Settings:
    names = {f.name for f in fields(Settings)}
    values = {k: v for k, v in values.items() if k in names}
    if isinstance(values.get("siri_model"), str):
        values["siri_model"] = [values["siri_model"]]
    return Settings(**values)


def make_settings(args: argparse.Namespace) -> Settings:
    names = {f.name for f in fields(Settings)}
    values = {}
    if args.config:
        for k, v in load_config(args.config).items():
            if k not in names:
                raise SystemExit(f"unknown key in {args.config}: {k}")
            values[k] = v
    for k, v in vars(args).items():
        if k in names:
            values[k] = v
    if isinstance(values.get("siri_model"), str):
        values["siri_model"] = [values["siri_model"]]
    return Settings(**values)


def _read_wav(path):
    import numpy as np
    from scipy import signal
    from scipy.io import wavfile
    sr, x = wavfile.read(path)
    if x.dtype == np.int16:
        x = x / 32768.0
    elif x.dtype == np.int32:
        x = x / 2147483648.0
    x = np.asarray(x, np.float64)
    if x.ndim == 2:
        x = x.mean(axis=1)
    if sr != FS:
        from math import gcd
        g = gcd(sr, FS)
        x = signal.resample_poly(x, FS // g, sr // g)
    return x.astype(np.float32)


def _write_wav(path, x):
    import numpy as np
    from scipy.io import wavfile
    wavfile.write(path, FS, np.clip(np.asarray(x) * 32767, -32768, 32767).astype(np.int16))


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    st = make_settings(args)

    if args.list_engines:
        from .aec import available_engines
        print("available AEC engines:", ", ".join(available_engines()))
        return 0

    if args.sim_mic or args.sim_ref:
        if not (args.sim_mic and args.sim_ref and args.sim_out):
            raise SystemExit("--sim-mic, --sim-ref and --sim-out go together")
        from .aec import AecProcessor, make_engine
        mic, ref = _read_wav(args.sim_mic), _read_wav(args.sim_ref)
        eng = make_engine(st.engine, noise_suppression=st.noise_suppression,
                          high_pass_filter=st.high_pass, auto_gain_control=st.agc,
                          filter_ms=st.filter_ms)
        proc = AecProcessor(eng, st.delay_ms, st.mic_delay_ms, st.output_gain_db)
        _write_wav(args.sim_out, proc.process_signal(mic, ref))
        print(f"[sim] engine={eng.name} wrote {args.sim_out}")
        return 0

    if args.sim_guard_in:
        if not args.sim_guard_out:
            raise SystemExit("--sim-guard-in needs --sim-guard-out")
        from .siri_guard import NotchConfig, make_guard
        g = make_guard(st.siri_mode, st.siri_model, st.siri_threshold, st.siri_lookahead_s,
                       NotchConfig.profile(st.siri_profile, depth_db=st.siri_depth_db))
        _write_wav(args.sim_guard_out, g.process_signal(_read_wav(args.sim_guard_in)))
        print(f"[sim] siri guard mode={st.siri_mode}: {g.detections} region(s) processed; "
              f"wrote {args.sim_guard_out}")
        return 0

    if args.rename_vbcable:
        from .vcable import rename_endpoints
        for line in rename_endpoints(restore=args.rename_vbcable == "restore") or ["nothing to change"]:
            print(line)
        return 0

    from .io_backend import SoundcardBackend
    backend = SoundcardBackend()

    if args.list_devices:
        devs = backend.list_devices()
        for kind, title in (("mic", "Microphones (--mic, --call-in)"),
                            ("speaker", "Speakers / outputs (--speaker, --out; loopback works on these)")):
            print(title + ":")
            for d in devs:
                if d.kind == kind:
                    print(f"  {'*' if d.default else ' '} {d.name}  [{d.channels} ch]")
        print("(* = system default)")
        return 0

    if args.calibrate:
        from .app import calibrate
        d, conf = calibrate(st, backend)
        if conf < 5:
            print("low confidence: raise speaker volume / move mic closer and retry")
        elif d >= 0:
            print(f"suggestion: webrtc needs nothing (AEC3 estimates delay itself); "
                  f"for speex/nlms with a long delay use --delay-ms {max(0.0, d - 20):.0f}")
        else:
            print(f"reference arrives late: use --mic-delay-ms {-d + 10:.0f}")
        return 0

    from .app import EchoGuardApp
    from .vcable import display_name
    app = EchoGuardApp(st, backend)
    devs = backend.list_devices()
    print(f"echoguard {__version__}: mic={st.mic or '(default)'} speaker={st.speaker or '(default)'}"
          f" -> {st.out}.  In Discord/LINE choose '{display_name(devs, 'virtual_mic')}' as the "
          f"microphone.  Ctrl+C stops.")
    if st.siri_guard:
        print(f"siri guard: set Discord/LINE OUTPUT device to "
              f"'{display_name(devs, 'call_speaker')}'.")
    app.run(args.duration_s)
    return 0


if __name__ == "__main__":
    sys.exit(main())
