"""How do the anti-trigger profiles affect *real* wake-word detectors?

Apple's detector is not available, so we use the open-source openWakeWord
models (alexa, hey_jarvis, hey_mycroft) as proxies: TTS speech of each wake
phrase (two voices) is processed with each profile and the max detector
score (0..1, default trigger threshold 0.5) is compared with the original.
Intelligibility is tracked with STOI (pystoi) and log-spectral distance.

    pip install piper-tts openwakeword onnxruntime pystoi
    python experiments/guard_proxy_eval.py --cache .cache
"""

import argparse
import os
import sys

import numpy as np
from scipy import signal

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _tts import oww_models, tts  # noqa: E402
from echoguard.metrics import log_spectral_distance, stoi  # noqa: E402
from echoguard.siri_guard import PROFILES, NotchBank, NotchConfig  # noqa: E402

WORDS = {"alexa": "Alexa", "hey_jarvis": "Hey Jarvis", "hey_mycroft": "Hey Mycroft"}


def max_score(model, x48):
    model.reset()
    x16 = signal.resample_poly(x48, 1, 3)
    x16 = np.concatenate((np.zeros(16000), x16, np.zeros(16000)))
    pcm = np.clip(x16 * 32767, -32768, 32767).astype(np.int16)
    best = 0.0
    for i in range(0, len(pcm) - 1280, 1280):
        best = max(best, max(model.predict(pcm[i:i + 1280]).values()))
    return best


def notch(x, cfg):
    nb = NotchBank(cfg)
    y = nb.process(np.concatenate((x, np.zeros(nb.delay, np.float32))))
    return y[nb.delay:]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", default=".cache")
    args = ap.parse_args()
    from openwakeword.model import Model
    md = oww_models(args.cache)
    configs = [("none", None)] + [(p, NotchConfig.profile(p)) for p in PROFILES]
    rows = []
    for key, phrase in WORDS.items():
        model = Model(wakeword_models=[os.path.join(md, f"{key}_v0.1.onnx")],
                      inference_framework="onnx",
                      melspec_model_path=os.path.join(md, "melspectrogram.onnx"),
                      embedding_model_path=os.path.join(md, "embedding_model.onnx"))
        for v in ("lessac", "ryan"):
            x = tts(args.cache, v, f"{phrase}, what's the weather like today?", key)
            x = (x / np.max(np.abs(x)) * 0.5).astype(np.float32)
            row = [f"{key}/{v}"]
            for name, cfg in configs:
                y = x if cfg is None else notch(x, cfg)
                s = max_score(model, y)
                st = 1.0 if cfg is None else stoi(x, y)
                row.append((name, s, st))
            rows.append(row)
    names = [n for n, _ in configs]
    print("| phrase/voice | " + " | ".join(names) + " |")
    print("|---" * (len(names) + 1) + "|")
    for r in rows:
        print(f"| {r[0]} | " + " | ".join(f"{s:.2f}" + ("" if n == "none" else f" (STOI {st:.2f})")
                                          for n, s, st in r[1:]) + " |")
    # Hey Siri itself: spectral effect only
    print("\n'Hey Siri' TTS, per profile: STOI / log-spectral distance (dB)")
    for v in ("lessac", "ryan"):
        x = tts(args.cache, v, "Hey Siri, what's the weather like today?", "hey_siri")
        for p in PROFILES:
            y = notch(x, NotchConfig.profile(p))
            print(f"  {v:7s} {p:11s} STOI={stoi(x, y):.3f}  LSD={log_spectral_distance(x, y):.2f} dB")


if __name__ == "__main__":
    main()
