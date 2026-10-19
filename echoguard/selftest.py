"""Quick self-check of the installed / frozen program (no audio devices
needed): which AEC engines load, offline ERLE on a synthetic echo, the Siri
notch, and whether the Windows audio API answers.

    python -m echoguard.selftest [report.txt]
    DeskEchoGuard.exe --self-test report.txt
"""

from __future__ import annotations

import platform
import sys
import traceback


def run() -> tuple[bool, list[str]]:
    lines = [f"python {sys.version.split()[0]} on {platform.platform()}",
             f"frozen: {bool(getattr(sys, 'frozen', False))}"]
    ok = True
    try:
        from .aec import AecProcessor, available_engines, make_engine
        from .metrics import band_energy_db, erle_db
        from .signals import make_scenario, synth_speech
        from .siri_guard import make_guard

        engines = available_engines()
        lines.append("engines: " + ", ".join(engines))
        for need in ("webrtc", "speex"):
            if need not in engines:
                ok = False
                lines.append(f"FAIL: engine {need} not available")
        sc = make_scenario(dur_s=12.0, delay_ms=40)
        floor = {"webrtc": 25.0, "speex": 10.0, "nlms": 5.0}
        for eng in [e for e in engines if e in floor]:
            out = AecProcessor(make_engine(eng, noise_suppression=False)).process_signal(sc.mic, sc.ref)
            e = erle_db(sc.mic, out, sc.far_only[:len(out)], skip_s=2.0)
            good = e >= floor[eng]
            ok &= good
            lines.append(f"{'ok  ' if good else 'FAIL'} AEC {eng}: ERLE {e:.1f} dB")
        x = synth_speech(3.0)
        y = make_guard("always").process_signal(x)
        drop = band_energy_db(x, 3800, 4200) - band_energy_db(y, 3800, 4200)
        ok &= drop > 15
        lines.append(f"{'ok  ' if drop > 15 else 'FAIL'} siri notch 4 kHz: -{drop:.1f} dB")
    except Exception:
        ok = False
        lines.append("FAIL: " + traceback.format_exc())

    try:
        import openwakeword.model  # noqa: F401
        lines.append("ok   openwakeword importable (detect mode)")
    except Exception as e:
        lines.append(f"info openwakeword not available: {e}")

    try:  # informational only: CI machines have no sound devices
        from .io_backend import SoundcardBackend
        devs = SoundcardBackend().list_devices()
        lines.append(f"info audio devices: {len(devs)}")
        for d in devs:
            lines.append(f"       {d.kind:7s} {d.name}")
    except Exception as e:
        lines.append(f"info audio API not usable here: {type(e).__name__}: {e}")
    lines.append("RESULT: " + ("PASS" if ok else "FAIL"))
    return ok, lines


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    ok, lines = run()
    text = "\n".join(lines) + "\n"
    if argv:
        with open(argv[0], "w", encoding="utf-8") as f:
            f.write(text)
    if sys.stdout is not None:
        print(text, end="")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
