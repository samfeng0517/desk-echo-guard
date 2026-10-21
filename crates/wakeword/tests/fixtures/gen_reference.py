"""Regenerate the reference fixtures for the Rust openWakeWord port.

Usage: python gen_reference.py MODEL_DIR
MODEL_DIR holds melspectrogram.onnx, embedding_model.onnx, hey_mycroft_v0.1.onnx,
alexa_v0.1.onnx, hey_mycroft_test.wav and alexa_test.wav (openWakeWord v0.5.1
release assets / tests/data).  Needs openwakeword>=0.6.0, onnxruntime and the
echoguard package (repo root on sys.path).

The random warm-up noise openWakeWord puts in its feature buffer is replaced
by the deterministic LCG noise the Rust port uses, so scores match from hop 0.
"""
import os
import sys
import wave

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..", "..", "..", "..", "..")))

from openwakeword.model import Model  # noqa: E402
from echoguard.resample import IntegerResampler  # noqa: E402
from echoguard.siri_guard import OpenWakeWordDetector  # noqa: E402


def lcg_noise(n, seed=0x5EED):
    """Must match `warmup_noise` in src/lib.rs."""
    s = seed
    out = np.empty(n, np.int16)
    for i in range(n):
        s = (s * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        out[i] = (s >> 33) % 2000 - 1000
    return out


def det_init(pre):
    pre.feature_buffer = pre._get_embeddings(lcg_noise(16000 * 4))


def read_wav(p):
    with wave.open(p) as w:
        return np.frombuffer(w.readframes(w.getnframes()), np.int16)


def main(d):
    feat = dict(melspec_model_path=os.path.join(d, "melspectrogram.onnx"),
                embedding_model_path=os.path.join(d, "embedding_model.onnx"))
    models = [os.path.join(d, "hey_mycroft_v0.1.onnx"), os.path.join(d, "alexa_v0.1.onnx")]
    z = np.zeros(16000, np.int16)
    clip = np.concatenate([z, read_wav(os.path.join(d, "hey_mycroft_test.wav")), z,
                           read_wav(os.path.join(d, "alexa_test.wav")), z])
    clip = clip[: len(clip) // 1280 * 1280]
    clip.astype("<i2").tofile(os.path.join(HERE, "clip16k.i16"))

    # 1) Model.predict on 80 ms hops
    m = Model(wakeword_models=list(models), inference_framework="onnx", **feat)
    det_init(m.preprocessor)
    with open(os.path.join(HERE, "model_scores_1280.txt"), "w") as f:
        for i in range(0, len(clip), 1280):
            s = m.predict(clip[i:i + 1280])
            f.write(" ".join(f"{float(v):.7g}" for v in s.values()) + "\n")

    # 2) irregular chunk sizes (remainder buffering, multi-hop calls)
    m = Model(wakeword_models=list(models), inference_framework="onnx", **feat)
    det_init(m.preprocessor)
    sizes = [1000, 1500, 2560, 700, 3000, 1280, 400, 4000]
    with open(os.path.join(HERE, "model_scores_irregular.txt"), "w") as f:
        i = k = 0
        while i < len(clip):
            n = sizes[k % len(sizes)]
            k += 1
            s = m.predict(clip[i:i + n])
            f.write(f"{len(clip[i:i + n])} " + " ".join(f"{float(v):.7g}" for v in s.values()) + "\n")
            i += n

    # 3) full detector at 48 kHz, 480-sample blocks
    up = IntegerResampler(-3).process(clip.astype(np.float32) / 32768.0)
    det = OpenWakeWordDetector(models[:1], threshold=0.5, inference_framework="onnx", **feat)
    det_init(det.model.preprocessor)
    hops = []
    predict = det.model.predict
    det.model.predict = lambda x: hops.append(1) or predict(x)
    with open(os.path.join(HERE, "detector_ref.txt"), "w") as f:
        for b in range(0, len(up) - 479, 480):
            n = len(hops)
            regions = det.feed(up[b:b + 480], b)
            assert len(hops) - n <= 1
            if len(hops) != n:
                f.write(f"hop {b} {float(max(det.last_scores.values())):.7g}\n")
            for a, e in regions:
                f.write(f"region {a} {e}\n")

if __name__ == "__main__":
    main(sys.argv[1])
