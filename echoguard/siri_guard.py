"""Pipeline B: "Hey Siri" anti-trigger processing for the call output.

What Apple appears to do (WWDC 2026 keynote, spectrogram analysis by audio
engineer Luuk de Leest, reported by MacRumors / heise / TechRadar; Apple has
not confirmed it): a *dynamic notch filter* that cuts narrow bands around
3, 4, 5 and 6 kHz only while the word "Siri" is spoken.  The word stays
intelligible (slightly thin/"metallic"), but the time-frequency pattern the
on-device wake-word network expects is broken.  See README for sources.

This module reproduces that as closely as the public information allows:

* :class:`NotchBank` - a linear-phase FIR band-stop at configurable centres
  (default 3/4/5/6 kHz), widths and depth.  Linear phase means the notched
  ("wet") signal is sample-aligned with the delayed dry signal, so we can
  cross-fade between them without comb filtering:
  ``out = dry - m(t) * (dry - wet)``.
* :class:`SiriGuard` - delays the stream by ``lookahead`` (~1 s), runs a
  detector on the *undelayed* input, and when the phrase is detected ramps
  ``m(t)`` to 1 over the phrase region (which is still inside the delay
  buffer), then back to 0.
* Detectors:
  - ``detect``   : openWakeWord with a custom "hey siri" model (train with
                   tools/train_hey_siri), closest to Apple's per-word notch.
  - ``sibilant`` : no model needed; notches for ~0.45 s from every strong
                   /s/-like burst (3.5-8 kHz energy onset).  "Siri" starts
                   with /s/, so it catches the phrase, but it also touches
                   other /s/ words (de-esser-like side effect).
  - ``always``   : apply the notches continuously (no detection, no
                   lookahead).  Simplest and most robust against triggers,
                   costs some brightness on all call audio.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np
from scipy import signal

from . import FS
from .buffers import DelayLine
from .resample import IntegerResampler


def _apple_bands(width=400.0):
    return tuple((c - width / 2, c + width / 2) for c in (3000.0, 4000.0, 5000.0, 6000.0))


# Stop-band profiles.  "apple" reproduces the reported WWDC 2026 notches
# (depth not public; 30 dB is our guess for "significantly reduced").  The
# others are stronger, more audible variants; their effect on open-source
# wake-word detectors is measured in experiments/guard_proxy_eval.py.
PROFILES = {
    "apple": {"bands": _apple_bands(400.0), "depth_db": 30.0},
    "apple-deep": {"bands": _apple_bands(400.0), "depth_db": 70.0},
    "apple-wide": {"bands": ((2800.0, 6500.0),), "depth_db": 50.0},
    "formant": {"bands": ((1000.0, 3000.0),) + _apple_bands(400.0), "depth_db": 70.0},
}


@dataclass
class NotchConfig:
    bands: tuple = PROFILES["apple"]["bands"]   # ((lo_hz, hi_hz), ...) stop bands
    depth_db: float = 30.0
    transition_hz: float = 150.0       # skirt on each side
    numtaps: int = 1201                # odd -> integer group delay (600 samples = 12.5 ms)

    @classmethod
    def profile(cls, name: str, **overrides) -> "NotchConfig":
        if name not in PROFILES:
            raise ValueError(f"unknown notch profile {name!r}; choose from {sorted(PROFILES)}")
        kw = dict(PROFILES[name])
        kw.update({k: v for k, v in overrides.items() if v is not None})
        return cls(**kw)


class NotchBank:
    """Streaming linear-phase FIR multi-band-stop (overlap-save via FFT)."""

    def __init__(self, cfg: NotchConfig | None = None, fs: int = FS):
        self.cfg = cfg = cfg or NotchConfig()
        self.fs = fs
        self.h = design_notch_fir(cfg, fs)
        self.delay = (len(self.h) - 1) // 2
        self._hist = np.zeros(len(self.h) - 1)

    def process(self, x: np.ndarray) -> np.ndarray:
        buf = np.concatenate((self._hist, np.asarray(x, np.float64)))
        y = signal.fftconvolve(buf, self.h, mode="valid")
        self._hist = buf[len(buf) - (len(self.h) - 1):]
        return y.astype(np.float32)


def notch_target_gain(cfg: NotchConfig, freqs: np.ndarray) -> np.ndarray:
    """Desired magnitude (linear) on ``freqs``: the minimum over stop bands
    of a trapezoid (1 outside, ``-depth_db`` inside, linear skirts), so
    overlapping bands merge cleanly."""
    g = 10 ** (-cfg.depth_db / 20)
    out = np.ones_like(freqs, dtype=np.float64)
    tr = max(cfg.transition_hz, 1.0)
    for lo, hi in cfg.bands:
        v = np.ones_like(out)
        v[(freqs >= lo) & (freqs <= hi)] = g
        up = (freqs > lo - tr) & (freqs < lo)
        v[up] = 1 - (1 - g) * (freqs[up] - (lo - tr)) / tr
        dn = (freqs > hi) & (freqs < hi + tr)
        v[dn] = g + (1 - g) * (freqs[dn] - hi) / tr
        out = np.minimum(out, v)
    return out


def design_notch_fir(cfg: NotchConfig, fs: int = FS) -> np.ndarray:
    freqs = np.linspace(0, fs / 2, 4097)
    return signal.firwin2(cfg.numtaps, freqs, notch_target_gain(cfg, freqs), fs=fs,
                          window=("kaiser", 6.0))

# detectors: feed(48 kHz block, start_index) -> list[(region_start, region_end)]
# in absolute input-sample indices
# --------------------------------------------------------------------------

class Detector:
    def feed(self, x: np.ndarray, start: int) -> list[tuple[int, int]]:
        raise NotImplementedError


class OracleDetector(Detector):
    """Test helper: fixed regions known in advance."""

    def __init__(self, regions):
        self.regions = list(regions)
        self._given = set()

    def feed(self, x, start):
        out = []
        end = start + len(x)
        for i, (a, b) in enumerate(self.regions):
            # "detect" when the phrase has fully arrived (like a real KWS)
            if i not in self._given and b <= end:
                self._given.add(i)
                out.append((a, b))
        return out


def _feature_model_paths(model_paths, framework) -> dict:
    """openWakeWord needs two shared feature models (melspectrogram and
    embedding).  Use copies next to the wake-word model if present, else the
    package's resources folder, downloading them there on first use."""
    import os
    import openwakeword
    ext = ".onnx" if framework == "onnx" else ".tflite"
    names = {"melspec_model_path": "melspectrogram" + ext,
             "embedding_model_path": "embedding_model" + ext}
    dirs = [os.path.dirname(os.path.abspath(p)) for p in model_paths]
    pkg = os.path.join(os.path.dirname(openwakeword.__file__), "resources", "models")
    out = {}
    for key, fn in names.items():
        for d in dirs + [pkg]:
            if os.path.exists(os.path.join(d, fn)):
                out[key] = os.path.join(d, fn)
                break
    if len(out) < 2:
        import openwakeword.utils
        target = dirs[0] if dirs else pkg
        for fm in openwakeword.FEATURE_MODELS.values():
            url = fm["download_url"].replace(".tflite", ext)
            if not os.path.exists(os.path.join(target, url.split("/")[-1])):
                openwakeword.utils.download_file(url, target)
        out = {k: os.path.join(target, fn) for k, fn in names.items()}
    return out


class OpenWakeWordDetector(Detector):
    """openWakeWord on a 16 kHz copy of the stream (80 ms hops).

    When the score crosses ``threshold`` at input index t, the phrase is the
    audio that just ended, so we mark [t - pre_s, t + post_s].
    """

    def __init__(self, model_paths, threshold=0.5, pre_s=0.9, post_s=0.15,
                 refractory_s=1.0, fs=FS, **oww_kwargs):
        from openwakeword.model import Model  # optional dependency
        kwargs = dict(oww_kwargs)
        paths = [str(p) for p in model_paths]
        if paths and all(p.endswith(".onnx") for p in paths):
            kwargs.setdefault("inference_framework", "onnx")
        kwargs.update(_feature_model_paths(paths, kwargs.get("inference_framework", "tflite")))
        self.model = Model(wakeword_models=paths, **kwargs)
        self.fs = fs
        self.threshold = threshold
        self.pre = int(pre_s * fs)
        self.post = int(post_s * fs)
        self.refractory = int(refractory_s * fs)
        self._down = IntegerResampler(fs // 16000)
        self._acc = np.zeros(0, np.float32)
        self._last = -10 ** 12
        self.last_scores: dict = {}

    def feed(self, x, start):
        self._acc = np.concatenate((self._acc, self._down.process(x)))
        out = []
        hop = 1280
        consumed_end = start + len(x)  # approx: detection time = end of this block
        while len(self._acc) >= hop:
            chunk, self._acc = self._acc[:hop], self._acc[hop:]
            pcm = np.clip(chunk * 32767, -32768, 32767).astype(np.int16)
            scores = self.model.predict(pcm)
            self.last_scores = scores
            t = consumed_end - int(len(self._acc) * self.fs / 16000)
            if max(scores.values(), default=0.0) >= self.threshold and t - self._last > self.refractory:
                self._last = t
                out.append((t - self.pre, t + self.post))
        return out


class SibilantDetector(Detector):
    """Training-free trigger on /s/-like onsets (high-band energy burst).

    10 ms frames.  A frame is sibilant when the 3.5-8 kHz band holds most of
    the energy and is well above its running floor.  On a sibilant onset at
    t we notch [t - pre_s, t + dur_s] ("Siri" ~ 0.35-0.5 s from its /s/)."""

    def __init__(self, ratio=0.5, rise_db=15.0, min_level_db=-55.0, pre_s=0.03,
                 dur_s=0.45, refractory_s=0.25, fs=FS):
        self.fs = fs
        self.ratio = ratio
        self.rise = 10 ** (rise_db / 10)
        self.min_level = 10 ** (min_level_db / 10)
        self.pre = int(pre_s * fs)
        self.dur = int(dur_s * fs)
        self.refractory = int(refractory_s * fs)
        self.hb = signal.butter(4, [3500, 8000], btype="band", fs=fs, output="sos")
        self.zi = signal.sosfilt_zi(self.hb) * 0
        self.floor = 1e-6
        self._prev = False
        self._last = -10 ** 12
        self.frame = fs // 100
        self._pending = np.zeros(0, np.float32)
        self._pending_start = 0

    def feed(self, x, start):
        if not len(self._pending):
            self._pending_start = start
        self._pending = np.concatenate((self._pending, x))
        out = []
        F = self.frame
        while len(self._pending) >= F:
            fr, self._pending = self._pending[:F], self._pending[F:]
            t = self._pending_start
            self._pending_start += F
            hb, self.zi = signal.sosfilt(self.hb, fr, zi=self.zi)
            e_hb = float(np.mean(hb ** 2))
            e = float(np.mean(fr.astype(np.float64) ** 2)) + 1e-12
            is_s = (e_hb > self.ratio * e and e_hb > self.rise * self.floor
                    and e_hb > self.min_level)
            if is_s and not self._prev and t - self._last > self.refractory:
                self._last = t
                out.append((t - self.pre, t + self.dur))
            self._prev = is_s
            # noise-floor tracker: follows dips at once, creeps up slowly
            self.floor = max(1e-10, e_hb if e_hb < self.floor else self.floor * 1.002)
        return out


# --------------------------------------------------------------------------

@dataclass
class GuardConfig:
    mode: str = "detect"          # detect | sibilant | always | off
    lookahead_s: float = 1.0
    ramp_ms: float = 15.0
    notch: NotchConfig = field(default_factory=NotchConfig)


class SiriGuard:
    """Streaming processor.  ``process(block) -> block`` (same length).

    Latency: notch group delay (12.5 ms default) + lookahead (0 in
    ``always`` mode)."""

    def __init__(self, cfg: GuardConfig | None = None, detector: Detector | None = None,
                 fs: int = FS):
        self.cfg = cfg = cfg or GuardConfig()
        self.fs = fs
        self.notch = NotchBank(cfg.notch, fs)
        self.lookahead = 0 if cfg.mode in ("always", "off") else int(cfg.lookahead_s * fs)
        self.dry_align = DelayLine(self.notch.delay)
        self.dry_look = DelayLine(self.lookahead)
        self.wet_look = DelayLine(self.lookahead)
        self.detector = detector
        if cfg.mode in ("detect", "sibilant") and detector is None:
            raise ValueError(f"mode {cfg.mode!r} needs a detector")
        self.ramp = max(1, int(cfg.ramp_ms * fs / 1000))
        self.latency = self.notch.delay + self.lookahead
        self.regions: list[tuple[int, int]] = []
        self.detections = 0
        self._t = 0  # absolute index of next input sample

    def _mask(self, s: int, n: int) -> np.ndarray:
        """Mask for *input* indices [s, s+n)."""
        if self.cfg.mode == "always":
            return np.ones(n, np.float32)
        m = np.zeros(n, np.float32)
        idx = np.arange(s, s + n)
        R = self.ramp
        keep = []
        for a, b in self.regions:
            if b + R < s:
                continue  # fully in the past
            keep.append((a, b))
            if a - R >= s + n:
                continue
            v = np.zeros(n, np.float32)
            v[(idx >= a) & (idx < b)] = 1.0
            up = (idx >= a - R) & (idx < a)
            v[up] = 0.5 - 0.5 * np.cos(np.pi * (idx[up] - (a - R)) / R)
            dn = (idx >= b) & (idx < b + R)
            v[dn] = 0.5 + 0.5 * np.cos(np.pi * (idx[dn] - b) / R)
            m = np.maximum(m, v)
        self.regions = keep
        return m

    def process(self, x: np.ndarray) -> np.ndarray:
        x = np.asarray(x, np.float32)
        n = len(x)
        if self.cfg.mode == "off":
            return x
        if self.detector is not None and self.cfg.mode in ("detect", "sibilant"):
            for a, b in self.detector.feed(x, self._t):
                # earliest input index still ahead of (or in) this output block
                earliest = self._t - self.latency
                if a < earliest:
                    a = earliest
                if b > a:
                    self.regions.append((a, b))
                    self.detections += 1
        wet = self.notch.process(x)
        dry = self.dry_align.process(x)
        wet = self.wet_look.process(wet)
        dry = self.dry_look.process(dry)
        # output sample k of this block corresponds to input index t + k - latency
        m = self._mask(self._t - self.latency, n)
        self._t += n
        return (dry - m * (dry - wet)).astype(np.float32)

    def process_signal(self, x: np.ndarray, block: int = 480, flush: bool = True) -> np.ndarray:
        """Offline helper; output is re-aligned to the input (latency removed)."""
        x = np.asarray(x, np.float32)
        pad = np.zeros(self.latency if flush else 0, np.float32)
        src = np.concatenate((x, pad))
        nb = int(np.ceil(len(src) / block))
        src = np.concatenate((src, np.zeros(nb * block - len(src), np.float32)))
        out = np.concatenate([self.process(src[i * block:(i + 1) * block]) for i in range(nb)])
        return out[self.latency:self.latency + len(x)]


def make_guard(mode: str = "detect", model_paths=(), threshold: float = 0.5,
               lookahead_s: float = 1.0, notch: NotchConfig | None = None,
               oww_kwargs: dict | None = None) -> SiriGuard:
    notch = notch or NotchConfig()
    cfg = GuardConfig(mode=mode, lookahead_s=lookahead_s, notch=notch)
    det = None
    if mode == "detect":
        if not model_paths:
            raise ValueError("--siri-mode detect needs --siri-model path/to/hey_siri.onnx "
                             "(see tools/train_hey_siri/README.md), or use --siri-mode "
                             "sibilant / always")
        pre = min(0.9, lookahead_s - 0.05)
        det = OpenWakeWordDetector(model_paths, threshold=threshold, pre_s=pre,
                                   **(oww_kwargs or {}))
    elif mode == "sibilant":
        det = SibilantDetector()
    return SiriGuard(cfg, det)
