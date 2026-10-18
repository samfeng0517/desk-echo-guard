"""Score wav files with a trained openWakeWord model (e.g. hey_siri.onnx).

    python tools/train_hey_siri/check_model.py hey_siri.onnx clip1.wav clip2.wav ...

Prints the max score per file and the time it occurs.  Use it to pick
--siri-threshold: real "Hey Siri" clips should score well above it, normal
conversation well below.
"""

import sys

import numpy as np
from scipy import signal
from scipy.io import wavfile


def load16k(path):
    sr, x = wavfile.read(path)
    x = x.astype(np.float32)
    if x.ndim == 2:
        x = x.mean(axis=1)
    if np.abs(x).max() > 1.5:
        x /= 32768.0
    from math import gcd
    g = gcd(sr, 16000)
    return signal.resample_poly(x, 16000 // g, sr // g)


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    from openwakeword.model import Model
    path = sys.argv[1]
    kw = {"inference_framework": "onnx"} if path.endswith(".onnx") else {}
    model = Model(wakeword_models=[path], **kw)
    for f in sys.argv[2:]:
        model.reset()
        x = np.concatenate((np.zeros(16000), load16k(f), np.zeros(16000)))
        pcm = np.clip(x * 32767, -32768, 32767).astype(np.int16)
        best, t_best = 0.0, 0.0
        for i in range(0, len(pcm) - 1280, 1280):
            s = max(model.predict(pcm[i:i + 1280]).values())
            if s > best:
                best, t_best = s, (i + 1280) / 16000 - 1.0
        print(f"{f}: max score {best:.3f} at {t_best:.2f} s")


if __name__ == "__main__":
    main()
