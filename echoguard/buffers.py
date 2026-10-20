"""Thread-safe buffers that connect audio devices running on different clocks.

Every physical / virtual device (mic, speaker loopback, VB-Cable) has its own
crystal, so two streams that are nominally 48 kHz differ by tens to hundreds of
ppm.  Left alone, the reference stream slowly slides against the mic stream
(the AEC sees a drifting delay) and output FIFOs grow or starve.

:class:`ElasticBuffer` absorbs that: the producer pushes whatever it gets, the
consumer pops exactly ``n`` samples, and the read side resamples by a ratio
``1 + u`` (|u| <= max_ppm, cubic interpolation).  ``u`` comes from a
critically damped PI controller on the fill level, so the level settles at
``target`` and the integral term converges to the true clock ratio.

Devices deliver fixed-size packets, so the raw fill level is a staircase.
The controller uses a *continuous* level estimate instead: the producer's
progress since its last push is interpolated from wall-clock time
(``now - t_last_push``), which removes the packet-size sawtooth.
Steady state: continuous level = target, i.e. the raw level averages
target + packet/2.  Large jumps (a producer that stalls and then resumes)
are handled by hard resync / re-priming instead of slow steering.
"""

from __future__ import annotations

import threading
import time

import numpy as np


class ElasticBuffer:
    def __init__(self, target: int, max_ppm: float = 1000.0,
                 hard_excess: int | None = None, tau_s: float = 6.0,
                 smooth_s: float = 0.5, fs: int = 48000, clock=time.perf_counter):
        """
        target: desired fill level in samples (this is also added latency).
        max_ppm: max resampling correction.
        hard_excess: if the level exceeds target + hard_excess, drop samples
            immediately back to target (default: 4 * target, >= 2400).
        tau_s: controller time constant (critically damped PI loop).
        smooth_s: time constant of the level-estimate low-pass (timing jitter).
        clock: time source in seconds (injectable for deterministic tests).
        """
        self.target = int(target)
        self.max_dev = max_ppm * 1e-6
        self.hard_excess = int(hard_excess if hard_excess is not None
                               else max(4 * self.target, 2400))
        self.fs = fs
        # e'' = -fs*kp*e' - fs*ki*e  ->  w = 1/tau, zeta = 1
        self.kp = 2.0 / (tau_s * fs)
        self.ki = 1.0 / (tau_s * tau_s * fs)
        self.smooth_s = smooth_s
        self.clock = clock
        self._lock = threading.Lock()
        # one sample of history in front of the read position for the cubic kernel
        self._data = np.zeros(1, dtype=np.float32)
        self._pos = 1.0
        self._primed = False
        self._level_f = 0.0
        self._integ = 0.0
        self._t_push = None
        self._last_chunk = 0
        self.ratio = 1.0
        # diagnostics
        self.underruns = 0
        self.resyncs = 0
        self.pushed = 0
        self.popped = 0

    # -- producer -----------------------------------------------------------
    def push(self, x: np.ndarray, now: float | None = None) -> None:
        x = np.asarray(x, dtype=np.float32).reshape(-1)
        with self._lock:
            self._data = np.concatenate((self._data, x))
            self.pushed += len(x)
            self._t_push = self.clock() if now is None else now
            self._last_chunk = len(x)

    # -- consumer -----------------------------------------------------------
    @property
    def level(self) -> float:
        with self._lock:
            return len(self._data) - self._pos

    def pop(self, n: int, now: float | None = None) -> np.ndarray:
        with self._lock:
            return self._pop_locked(n, self.clock() if now is None else now)

    def _continuous_level(self, now: float) -> float:
        level = len(self._data) - self._pos
        if self._t_push is None:
            return level
        since = max(0.0, now - self._t_push) * self.fs
        return level - self._last_chunk + min(since, self._last_chunk)

    def _reset_to_target(self, now: float):
        """Jump the read position so the continuous level equals target."""
        self._pos += self._continuous_level(now) - self.target
        self._level_f = float(self.target)

    def _pop_locked(self, n: int, now: float) -> np.ndarray:
        level = len(self._data) - self._pos
        if not self._primed:
            if self._continuous_level(now) < self.target:
                return np.zeros(n, dtype=np.float32)
            # start exactly at target for a reproducible latency
            self._reset_to_target(now)
            self._primed = True
        elif level > self.target + self.hard_excess:
            self._reset_to_target(now)
            self.resyncs += 1

        dt = n / self.fs
        a = min(1.0, dt / self.smooth_s) if self.smooth_s > 0 else 1.0
        cl = self._continuous_level(now)
        self._level_f += a * (cl - self._level_f)
        e = self._level_f - self.target
        # anti-windup: an underrunning producer says nothing about its clock
        if len(self._data) - self._pos >= n:
            self._integ = min(self.max_dev, max(-self.max_dev, self._integ + self.ki * e * dt))
        u = min(self.max_dev, max(-self.max_dev, self.kp * e + self._integ))
        self.ratio = 1.0 + u

        p = self._pos + self.ratio * np.arange(n)
        i = np.floor(p).astype(np.int64)
        t = (p - i).astype(np.float32)
        d = self._data
        short = int(i[-1]) + 3 - len(d)
        if short > 0:
            # Underrun: the consumer's clock is the master, so time keeps
            # running - missing samples are played as silence and the read
            # position still advances.  Late samples that arrive for that
            # span are skipped, which keeps ref/mic alignment unchanged.
            self.underruns += 1
            d = np.concatenate((d, np.zeros(short, np.float32)))
        y0, y1, y2, y3 = d[i - 1], d[i], d[i + 1], d[i + 2]
        # Catmull-Rom spline
        c3 = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3
        c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3
        c1 = -0.5 * y0 + 0.5 * y2
        out = ((c3 * t + c2) * t + c1) * t + y1

        self._pos += self.ratio * n
        drop = min(int(self._pos) - 1, len(self._data) - 1)
        if drop > 4096:
            self._data = self._data[drop:]
            self._pos -= drop
        if len(self._data) - self._pos < -self.hard_excess:
            # producer stopped for a long time: start over (re-prime)
            self._data = self._data[-1:]
            self._pos = 1.0
            self._primed = False
            self._t_push = None
            self._integ = 0.0
            self.resyncs += 1
        self.popped += n
        return out.astype(np.float32)

    def stats(self) -> dict:
        with self._lock:
            return {"level": len(self._data) - self._pos,
                    "ppm": (self.ratio - 1.0) * 1e6,
                    "drift_ppm": self._integ * 1e6,
                    "underruns": self.underruns,
                    "resyncs": self.resyncs}


class DelayLine:
    """Integer-sample streaming delay (ring buffer: a block costs O(block),
    not O(delay), which matters for the ~1 s Siri-guard lookahead)."""

    def __init__(self, delay: int):
        self.delay = int(max(0, delay))
        self._buf = np.zeros(self.delay, dtype=np.float32)
        self._w = 0

    def process(self, x: np.ndarray) -> np.ndarray:
        x = np.asarray(x, dtype=np.float32)
        d, n = self.delay, len(x)
        if d == 0:
            return x
        if n >= d:
            out = np.concatenate((self._buf[self._w:], self._buf[:self._w], x[:n - d]))
            self._buf[:] = x[n - d:]
            self._w = 0
            return out
        idx = np.arange(self._w, self._w + n) % d if self._w + n > d else slice(self._w, self._w + n)
        out = self._buf[idx].copy()
        self._buf[idx] = x
        self._w = (self._w + n) % d
        return out
