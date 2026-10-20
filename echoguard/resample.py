"""Streaming integer-ratio resampler (48k <-> 16k / 24k) with filter state."""

from __future__ import annotations

import numpy as np
from scipy import signal


class IntegerResampler:
    """Stateful up- or down-sampler by an integer factor.

    ``factor > 0``: downsample by ``factor``; ``factor < 0``: upsample by ``-factor``.
    Linear-phase FIR anti-alias / anti-image filter, state kept across calls,
    so feeding a signal in arbitrary block sizes gives the same result as
    feeding it in one go (block length must be a multiple of the factor when
    downsampling).
    """

    def __init__(self, factor: int, taps_per_phase: int = 32):
        if factor == 0:
            raise ValueError("factor must be non-zero")
        self.down = factor > 0
        self.k = abs(int(factor))
        n = taps_per_phase * self.k + 1
        self.h = signal.firwin(n, 0.9 / self.k).astype(np.float64)
        if not self.down:
            self.h *= self.k
        self.zi = np.zeros(len(self.h) - 1)
        self._phase = 0
        self.delay = (len(self.h) - 1) // 2  # in high-rate samples

    def process(self, x: np.ndarray) -> np.ndarray:
        x = np.asarray(x, dtype=np.float64)
        if self.k == 1:
            return x.astype(np.float32)
        if self.down:
            y, self.zi = signal.lfilter(self.h, 1.0, x, zi=self.zi)
            out = y[self._phase::self.k]
            self._phase = (self._phase - len(x)) % self.k
            return out.astype(np.float32)
        up = np.zeros(len(x) * self.k)
        up[::self.k] = x
        y, self.zi = signal.lfilter(self.h, 1.0, up, zi=self.zi)
        return y.astype(np.float32)
