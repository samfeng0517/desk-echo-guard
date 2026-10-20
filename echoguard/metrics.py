"""Objective measures used by the offline tests / experiments."""

from __future__ import annotations

import numpy as np
from scipy import signal

from . import FS


def db(x: float) -> float:
    return float(10 * np.log10(max(x, 1e-20)))


def energy(x: np.ndarray) -> float:
    return float(np.mean(np.asarray(x, dtype=np.float64) ** 2))


def erle_db(mic: np.ndarray, out: np.ndarray, mask: np.ndarray | None = None,
            skip_s: float = 0.0, fs: int = FS) -> float:
    """Echo return loss enhancement: 10 log10(E[mic^2] / E[out^2]) over a
    far-end-only region (mask), optionally skipping the first skip_s
    seconds of that region (convergence)."""
    if mask is None:
        mask = np.ones(len(out), bool)
    mask = mask[:len(out)].copy()
    if skip_s > 0:
        first = np.argmax(mask)
        mask[first:first + int(skip_s * fs)] = False
    return db(energy(mic[:len(out)][mask])) - db(energy(out[mask]))


def best_lag(a: np.ndarray, b: np.ndarray, max_lag: int) -> int:
    """Lag (samples) by which b is delayed relative to a."""
    c = signal.correlate(b, a, mode="full", method="fft")
    mid = len(a) - 1
    seg = c[mid - max_lag: mid + max_lag + 1]
    return int(np.argmax(np.abs(seg)) - max_lag)


def snr_db(ref: np.ndarray, test: np.ndarray, align_max: int = 0) -> float:
    """SNR of test against ref after optional integer alignment and an
    optimal scalar gain (so plain level changes are not counted)."""
    ref = np.asarray(ref, np.float64)
    test = np.asarray(test, np.float64)
    if align_max:
        lag = best_lag(ref, test, align_max)
        if lag > 0:
            test = test[lag:]
        elif lag < 0:
            ref = ref[-lag:]
    n = min(len(ref), len(test))
    ref, test = ref[:n], test[:n]
    g = np.dot(ref, test) / max(np.dot(ref, ref), 1e-20)
    err = test - g * ref
    return db(np.dot(g * ref, g * ref)) - db(np.dot(err, err))


def band_energy_db(x: np.ndarray, lo: float, hi: float, fs: int = FS) -> float:
    f, p = signal.welch(x, fs=fs, nperseg=4096)
    sel = (f >= lo) & (f < hi)
    return db(float(np.sum(p[sel])))


def log_spectral_distance(a: np.ndarray, b: np.ndarray, fs: int = FS,
                          lo: float = 0, hi: float | None = None) -> float:
    """Mean over STFT frames of RMS(dB difference) between spectra, in dB."""
    hi = hi or fs / 2
    n = min(len(a), len(b))
    f, _, A = signal.stft(a[:n], fs=fs, nperseg=1024)
    _, _, B = signal.stft(b[:n], fs=fs, nperseg=1024)
    sel = (f >= lo) & (f <= hi)
    pa = np.abs(A[sel]) ** 2 + 1e-10
    pb = np.abs(B[sel]) ** 2 + 1e-10
    active = pa.sum(axis=0) > 1e-6 * pa.sum(axis=0).max()
    d = 10 * np.log10(pa[:, active] / pb[:, active])
    return float(np.mean(np.sqrt(np.mean(d ** 2, axis=0))))


def envelope_correlation(ref: np.ndarray, test: np.ndarray, fs: int = FS) -> float:
    """Cheap intelligibility proxy (STOI-like): mean correlation of 1/3-octave
    band envelopes (150 Hz .. 8 kHz) between clean and processed speech,
    over 384 ms segments.  1.0 = identical envelopes."""
    n = min(len(ref), len(test))
    ref, test = ref[:n], test[:n]
    f, _, R = signal.stft(ref, fs=fs, nperseg=1024, noverlap=512)
    _, _, T = signal.stft(test, fs=fs, nperseg=1024, noverlap=512)
    centers = 150 * 2 ** (np.arange(18) / 3)
    centers = centers[centers < 8000]
    seg = 36  # frames of 10.7 ms -> ~384 ms
    cs = []
    for fc in centers:
        sel = (f >= fc * 2 ** (-1 / 6)) & (f < fc * 2 ** (1 / 6))
        er = np.sqrt(np.sum(np.abs(R[sel]) ** 2, axis=0))
        et = np.sqrt(np.sum(np.abs(T[sel]) ** 2, axis=0))
        for s in range(0, len(er) - seg, seg // 2):
            a, b = er[s:s + seg], et[s:s + seg]
            if a.std() < 1e-6 or a.max() < 1e-4 * er.max():
                continue
            if b.std() < 1e-9:
                cs.append(0.0)
                continue
            cs.append(float(np.corrcoef(a, b)[0, 1]))
    return float(np.mean(cs)) if cs else 0.0


def align(ref: np.ndarray, test: np.ndarray, max_lag: int = 2400):
    """Remove the processing latency of ``test`` relative to ``ref``."""
    lag = best_lag(ref, test, max_lag)
    if lag > 0:
        test = test[lag:]
    elif lag < 0:
        ref = ref[-lag:]
    n = min(len(ref), len(test))
    return ref[:n], test[:n]


def stoi(ref: np.ndarray, test: np.ndarray, fs: int = FS, max_lag: int = 2400) -> float | None:
    """Real STOI (Taal et al. 2011) if the optional ``pystoi`` package is
    installed; ``test`` is aligned to ``ref`` first."""
    try:
        from pystoi import stoi as _stoi
    except Exception:
        return None
    ref, test = align(np.asarray(ref, np.float64), np.asarray(test, np.float64), max_lag)
    n = min(len(ref), len(test))
    return float(_stoi(ref[:n], test[:n], fs, extended=False))
