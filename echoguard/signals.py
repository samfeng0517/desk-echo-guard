"""Synthetic test signals (no recordings needed): speech-like source,
room impulse response, and an echo scenario mic = near + h * ref + noise."""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
from scipy import signal

from . import FS

# (F1, F2, F3) for a handful of vowels, Hz
_VOWELS = [(730, 1090, 2440), (270, 2290, 3010), (300, 870, 2240),
           (530, 1840, 2480), (570, 840, 2410), (440, 1020, 2240),
           (660, 1720, 2410)]


def synth_speech(dur_s: float, fs: int = FS, f0: float = 120.0, seed: int = 0,
                 pause_prob: float = 0.15) -> np.ndarray:
    """Speech-like signal: jittered glottal pulse train through time-varying
    formant resonators, syllabic envelope (~4 Hz), plus fricative noise
    bursts (energy 3-8 kHz, like /s/) and pauses.  Normalised to RMS 0.05 (peaks ~ -6 dBFS)
    over active parts."""
    rng = np.random.default_rng(seed)
    n = int(dur_s * fs)
    out = np.zeros(n)
    pos = 0
    while pos < n:
        syl = int(rng.uniform(0.15, 0.32) * fs)
        seg_end = min(n, pos + syl)
        L = seg_end - pos
        if rng.random() < pause_prob:
            pos = seg_end
            continue
        if rng.random() < 0.3:
            # fricative: high-passed noise, 3-8 kHz
            b, a = signal.butter(4, [3000, 8000], btype="band", fs=fs)
            seg = signal.lfilter(b, a, rng.standard_normal(L))
            seg *= 0.3 / np.std(seg)  # fricatives ~10 dB below vowels
        else:
            # intonation contour (+-30 %), per-period jitter, shimmer
            f = f0 * (1 + 0.3 * np.sin(2 * np.pi * rng.uniform(0.5, 2) * np.arange(L) / fs
                                       + rng.uniform(0, 6.28)))
            phase = np.cumsum(f / fs)
            k = np.floor(phase).astype(int)
            pulses = np.diff(k, prepend=k[0]).astype(float)
            pulses *= 1 + 0.2 * rng.standard_normal(L)
            jit = rng.integers(-int(0.0005 * fs), int(0.0005 * fs) + 1, L)
            pulses = np.bincount(np.clip(np.arange(L) + jit * (pulses != 0), 0, L - 1),
                                 weights=pulses, minlength=L)
            src = signal.lfilter([1.0], [1.0, -0.95], pulses)  # glottal tilt
            breath = signal.lfilter([1.0], [1.0, -0.7], rng.standard_normal(L))
            src = src + 0.25 * np.std(src) / np.std(breath) * breath  # aspiration
            # formants glide between two vowels
            F0v = np.array(_VOWELS[rng.integers(len(_VOWELS))], float)
            F1v = np.array(_VOWELS[rng.integers(len(_VOWELS))], float)
            seg = np.zeros(L)
            nsub = 8
            edges = np.linspace(0, L, nsub + 1).astype(int)
            zis = [np.zeros(2) for _ in range(3)]
            for j in range(nsub):
                part = src[edges[j]:edges[j + 1]]
                F = F0v + (F1v - F0v) * (j + 0.5) / nsub
                for q, (fc, bw) in enumerate(zip(F, (80, 100, 140))):
                    r = np.exp(-np.pi * bw / fs)
                    th = 2 * np.pi * fc / fs
                    part, zis[q] = signal.lfilter([1 - r], [1, -2 * r * np.cos(th), r * r],
                                                  part, zi=zis[q])
                seg[edges[j]:edges[j + 1]] = part
            seg /= np.std(seg) + 1e-12
        env = np.sin(np.pi * np.arange(L) / L) ** 0.6
        out[pos:seg_end] = seg * env
        pos = seg_end
    active = np.abs(out) > 1e-6
    rms = np.sqrt(np.mean(out[active] ** 2)) if active.any() else 1.0
    return (out * 0.05 / rms).astype(np.float32)


def room_ir(fs: int = FS, length_ms: float = 150.0, rt60_ms: float = 200.0,
            direct_delay_ms: float = 3.0, gain: float = 0.5, drr_db: float = 0.0,
            seed: int = 1) -> np.ndarray:
    """Direct path (speaker -> mic) + exponentially decaying noise tail
    (reflections).  ``drr_db`` = direct-to-reverberant energy ratio; a
    desktop speaker ~0.5-1 m from the mic in a small room is roughly
    -3..+6 dB.  ``gain`` = peak amplitude of the IR (acoustic coupling)."""
    rng = np.random.default_rng(seed)
    n = int(fs * length_ms / 1000)
    d = int(fs * direct_delay_ms / 1000)
    t = np.arange(n - d - 1) / fs
    tail = rng.standard_normal(len(t)) * np.exp(-6.9 * t / (rt60_ms / 1000))
    tail *= np.sqrt(10 ** (-drr_db / 10) / np.sum(tail ** 2))
    h = np.zeros(n)
    h[d] = 1.0
    h[d + 1:] = tail
    b, a = signal.butter(2, [80, 12000], btype="band", fs=fs)  # speaker/mic response
    h = signal.lfilter(b, a, h)
    return (gain * h / np.max(np.abs(h))).astype(np.float32)


@dataclass
class EchoScenario:
    mic: np.ndarray
    ref: np.ndarray
    near: np.ndarray
    echo: np.ndarray
    far_only: np.ndarray    # boolean masks (per sample)
    near_only: np.ndarray
    double_talk: np.ndarray


def make_scenario(dur_s: float = 24.0, fs: int = FS, delay_ms: float = 40.0,
                  echo_gain: float = 0.5, noise_db: float = -60.0,
                  nonlinear: float = 0.0, seed: int = 0,
                  far: np.ndarray | None = None, near: np.ndarray | None = None) -> EchoScenario:
    """Layout: [0, 40%) far end only, [40%, 60%) near end only,
    [60%, 100%) double talk.  ``delay_ms`` is the extra (bulk) delay between
    the reference as seen by the canceller and its echo in the mic, on top
    of the IR's direct path.  ``far`` / ``near`` may be supplied (e.g. real
    recordings); they are tiled / truncated to ``dur_s``."""
    n = int(dur_s * fs)
    if far is None:
        far = synth_speech(dur_s, fs, f0=210, seed=seed + 10, pause_prob=0.1)
    if near is None:
        near = synth_speech(dur_s, fs, f0=110, seed=seed + 20)
    far = np.resize(np.asarray(far, np.float32), n)
    near_full = np.resize(np.asarray(near, np.float32), n)
    a, b = int(0.4 * n), int(0.6 * n)
    ref = far.copy()
    ref[a:b] = 0.0
    near = near_full.copy()
    near[:a] = 0.0
    h = room_ir(fs, gain=echo_gain, seed=seed + 1)
    drive = ref if nonlinear <= 0 else np.tanh(ref * (1 + 10 * nonlinear)) / (1 + 10 * nonlinear)
    echo = signal.fftconvolve(drive, h)[:n]
    dly = int(fs * delay_ms / 1000)
    echo = np.concatenate((np.zeros(dly), echo))[:n]
    rng = np.random.default_rng(seed + 30)
    noise = rng.standard_normal(n) * 10 ** (noise_db / 20)
    mic = (near + echo + noise).astype(np.float32)
    idx = np.arange(n)
    return EchoScenario(mic=mic, ref=ref.astype(np.float32), near=near.astype(np.float32),
                        echo=echo.astype(np.float32),
                        far_only=idx < a, near_only=(idx >= a) & (idx < b),
                        double_talk=idx >= b)
