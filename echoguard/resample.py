"""Streaming integer-ratio resampler (48k <-> 16k / 24k) with filter state.

Pure numpy (no scipy at run time): scipy.signal alone costs ~75 MB of RAM,
and the 16 kHz engines are the only real-time users of this module.
"""

from __future__ import annotations

import numpy as np
from numpy.lib.stride_tricks import sliding_window_view


def firwin_lowpass(numtaps: int, cutoff: float) -> np.ndarray:
    """``scipy.signal.firwin(numtaps, cutoff)``: Hamming-windowed sinc low-pass,
    cutoff relative to Nyquist, unity gain at DC."""
    m = np.arange(numtaps) - 0.5 * (numtaps - 1)
    h = cutoff * np.sinc(cutoff * m) * np.hamming(numtaps)
    return h / np.sum(h)


class IntegerResampler:
    """Stateful up- or down-sampler by an integer factor.

    ``factor > 0``: downsample by ``factor``; ``factor < 0``: upsample by ``-factor``.
    Linear-phase FIR anti-alias / anti-image filter, state kept across calls,
    so feeding a signal in arbitrary block sizes gives the same result as
    feeding it in one go (block length must be a multiple of the factor when
    downsampling).  Polyphase: only the output samples that are kept are
    computed, and the zeros of the upsampled signal are never multiplied.
    """

    def __init__(self, factor: int, taps_per_phase: int = 32):
        if factor == 0:
            raise ValueError("factor must be non-zero")
        self.down = factor > 0
        self.k = k = abs(int(factor))
        n = taps_per_phase * k + 1
        self.h = firwin_lowpass(n, 0.9 / k)
        if not self.down:
            self.h *= k
        self.delay = (len(self.h) - 1) // 2  # in high-rate samples
        self._phase = 0
        if self.down:
            self._hr = self.h[::-1].copy()
            self._hist = np.zeros(len(self.h) - 1)
        else:
            # H[m, p] = h[m*k + p]: output n*k + p = sum_m H[m, p] * x[n - m]
            m = -(-len(self.h) // k)
            hp = np.zeros(m * k)
            hp[:len(self.h)] = self.h
            self._hr = hp.reshape(m, k)[::-1].copy()
            self._hist = np.zeros(m - 1)

    def process(self, x: np.ndarray) -> np.ndarray:
        x = np.asarray(x, dtype=np.float64)
        if self.k == 1 or not len(x):
            return x.astype(np.float32)
        buf = np.concatenate((self._hist, x))
        self._hist = buf[len(x):]
        w = sliding_window_view(buf, len(self._hr))
        if self.down:
            y = w[self._phase::self.k] @ self._hr
            self._phase = (self._phase - len(x)) % self.k
        else:
            y = (w @ self._hr).reshape(-1)
        return y.astype(np.float32)
