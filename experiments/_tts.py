"""Shared helpers: fetch Piper voices / openWakeWord models and synthesise
speech into a cache directory (needs network + `pip install piper-tts
openwakeword onnxruntime`)."""

from __future__ import annotations

import os
import subprocess
import sys
import urllib.request

import numpy as np
from scipy import signal
from scipy.io import wavfile

VOICES = {
    "lessac": "en/en_US/lessac/medium/en_US-lessac-medium",
    "ryan": "en/en_US/ryan/medium/en_US-ryan-medium",
}
HF = "https://huggingface.co/rhasspy/piper-voices/resolve/main/"


def fetch(url: str, path: str) -> str:
    if not os.path.exists(path):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        urllib.request.urlretrieve(url, path)
    return path


def voice(cache: str, name: str) -> str:
    rel = VOICES[name]
    base = os.path.join(cache, "voices", os.path.basename(rel))
    fetch(HF + rel + ".onnx.json", base + ".onnx.json")
    return fetch(HF + rel + ".onnx", base + ".onnx")


def tts(cache: str, voice_name: str, text: str, tag: str) -> np.ndarray:
    """48 kHz float32 speech for ``text`` (cached as wav)."""
    out = os.path.join(cache, "tts", f"{tag}_{voice_name}.wav")
    if not os.path.exists(out):
        os.makedirs(os.path.dirname(out), exist_ok=True)
        subprocess.run([sys.executable, "-m", "piper", "-m", voice(cache, voice_name), "-f", out],
                       input=text.encode(), check=True, capture_output=True)
    sr, x = wavfile.read(out)
    x = x.astype(np.float32) / 32768.0
    from math import gcd
    g = gcd(sr, 48000)
    return signal.resample_poly(x, 48000 // g, sr // g).astype(np.float32)


def oww_models(cache: str) -> str:
    d = os.path.join(cache, "oww")
    if not os.path.exists(os.path.join(d, "embedding_model.onnx")):
        import openwakeword.utils
        os.makedirs(d, exist_ok=True)
        openwakeword.utils.download_models(target_directory=d)
    return d
