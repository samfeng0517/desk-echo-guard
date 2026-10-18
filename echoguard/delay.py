"""Reference -> mic delay estimation (GCC-PHAT)."""

from __future__ import annotations

import numpy as np

from . import FS


def estimate_delay_ms(ref: np.ndarray, mic: np.ndarray, fs: int = FS,
                      max_delay_ms: float = 500.0, min_delay_ms: float | None = None
                      ) -> tuple[float, float]:
    """Return (delay_ms, confidence).

    delay > 0: the echo shows up in ``mic`` that many ms *after* the same
    sound appears in ``ref`` (normal, causal case).  delay < 0: the reference
    arrives late - delay the mic by -delay (``--mic-delay-ms``).
    confidence = peak / mean(|cc|), roughly > 5 is trustworthy.
    """
    ref = np.asarray(ref, np.float64)
    mic = np.asarray(mic, np.float64)
    n = min(len(ref), len(mic))
    ref, mic = ref[:n], mic[:n]
    nfft = 1 << int(np.ceil(np.log2(2 * n)))
    R = np.fft.rfft(ref, nfft)
    M = np.fft.rfft(mic, nfft)
    G = M * np.conj(R)
    G /= np.abs(G) + 1e-12
    cc = np.fft.irfft(G, nfft)
    maxlag = int(max_delay_ms * fs / 1000)
    minlag = -maxlag if min_delay_ms is None else int(min_delay_ms * fs / 1000)
    lags = np.arange(minlag, maxlag + 1)
    vals = np.abs(cc[lags % nfft])
    k = int(np.argmax(vals))
    conf = float(vals[k] / (np.mean(vals) + 1e-12))
    return float(lags[k] * 1000.0 / fs), conf
