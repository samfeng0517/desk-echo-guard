"""Acoustic echo cancellation engines behind one interface.

All engines take 10 ms frames at 48 kHz (float32, mono, roughly -1..1) for the
microphone (near end + echo) and the speaker reference (far end), and return
the cleaned 10 ms mic frame at 48 kHz.  Engines that run internally at a lower
rate are wrapped with streaming resamplers.

Engines
-------
webrtc : WebRTC AudioProcessing (AEC3 + optional NS / HPF / AGC) via the
         ``livekit`` wheel (``livekit.rtc.AudioProcessingModule``), which ships
         native binaries for Windows / macOS / Linux.  Best quality.
speex  : SpeexDSP echo canceller (+ preprocessor / residual echo suppression)
         via the ``pyaec`` wheel (bundled aec.dll).  Runs at 16 kHz.
nlms   : Built-in pure-numpy partitioned-block frequency-domain adaptive filter
         (MDF-style, with Valin-style leakage-controlled step size).  Always
         available, linear only (no residual echo suppressor) -> a fallback /
         test reference.
"""

from __future__ import annotations

import numpy as np

from . import FS, FRAME
from .buffers import DelayLine
from .resample import IntegerResampler


def _to_i16(x: np.ndarray) -> np.ndarray:
    return np.clip(np.round(x * 32767.0), -32768, 32767).astype(np.int16)


def _from_i16(x) -> np.ndarray:
    return np.asarray(x, dtype=np.float32) / 32768.0


class EchoCanceller:
    """Base: process(mic_48k_10ms, ref_48k_10ms) -> out_48k_10ms."""

    name = "base"

    def process(self, mic: np.ndarray, ref: np.ndarray) -> np.ndarray:
        raise NotImplementedError


class PassThrough(EchoCanceller):
    name = "off"

    def process(self, mic, ref):
        return np.asarray(mic, dtype=np.float32)


class WebRtcAec(EchoCanceller):
    """WebRTC APM (AEC3) through livekit's FFI binding, at 48 kHz."""

    name = "webrtc"

    def __init__(self, noise_suppression=True, high_pass_filter=True,
                 auto_gain_control=False, stream_delay_ms=0):
        from livekit import rtc  # optional dependency
        self._rtc = rtc
        self.apm = rtc.AudioProcessingModule(
            echo_cancellation=True,
            noise_suppression=noise_suppression,
            high_pass_filter=high_pass_filter,
            auto_gain_control=auto_gain_control,
        )
        self.stream_delay_ms = int(stream_delay_ms)

    def _frame(self, x):
        return self._rtc.AudioFrame(_to_i16(x).tobytes(), FS, 1, FRAME)

    def process(self, mic, ref):
        rf = self._frame(ref)
        self.apm.process_reverse_stream(rf)
        mf = self._frame(mic)
        self.apm.set_stream_delay_ms(self.stream_delay_ms)
        self.apm.process_stream(mf)
        return _from_i16(np.frombuffer(mf.data, dtype=np.int16))


class _Resampled(EchoCanceller):
    """Runs a 16 kHz engine inside the 48 kHz pipeline."""

    rate = 16000

    def __init__(self):
        k = FS // self.rate
        self._dm = IntegerResampler(k)
        self._dr = IntegerResampler(k)
        self._up = IntegerResampler(-k)

    def process(self, mic, ref):
        m = self._dm.process(mic)
        r = self._dr.process(ref)
        return self._up.process(self.process_lowrate(m, r))

    def process_lowrate(self, mic, ref):
        raise NotImplementedError


class SpeexAec(_Resampled):
    name = "speex"

    def __init__(self, filter_ms=300, preprocess=True):
        super().__init__()
        import pyaec  # optional dependency
        self.n = self.rate // 100
        self.aec = pyaec.Aec(self.n, int(self.rate * filter_ms / 1000),
                             self.rate, bool(preprocess))

    def process_lowrate(self, mic, ref):
        out = self.aec.cancel_echo(_to_i16(mic).tolist(), _to_i16(ref).tolist())
        return _from_i16(out)


class NlmsAec(_Resampled):
    """Partitioned-block frequency-domain adaptive filter (overlap-save, MDF).

    Block N = 160 (10 ms @ 16 kHz), P partitions -> filter length P*N.
    Normalised per-bin step with a double-talk-robust factor
    mu_k = min(mu_max, |Y_k|^2 / |E_k|^2) (echo estimate vs. residual power;
    cf. Valin 2007, "On adjusting the learning rate in frequency domain echo
    cancellation with double-talk"): when the near end talks, |E| grows and
    adaptation slows down by itself.  A fixed step is used until the filter
    has converged a little (|Y| is 0 at start).  Linear only: no residual
    echo suppressor, so expect ~10-20 dB ERLE.
    """

    name = "nlms"

    def __init__(self, filter_ms=300, mu_max=0.8, mu_init=0.5):
        super().__init__()
        self.N = N = self.rate // 100
        self.P = P = max(1, int(round(filter_ms * self.rate / 1000 / N)))
        self.K = N + 1
        self.W = np.zeros((P, self.K), dtype=np.complex128)
        self.X = np.zeros((P, self.K), dtype=np.complex128)
        self.x_prev = np.zeros(N)
        self.Sxx = np.full(self.K, 1e-6)
        self.mu_max = mu_max
        self.mu_init = mu_init
        self.adapted = 0.0

    def process_lowrate(self, mic, ref):
        N = self.N
        d = np.asarray(mic, dtype=np.float64)
        x = np.asarray(ref, dtype=np.float64)
        Xf = np.fft.rfft(np.concatenate((self.x_prev, x)))
        self.x_prev = x
        self.X = np.roll(self.X, 1, axis=0)
        self.X[0] = Xf

        Y = np.sum(self.W * self.X, axis=0)
        y = np.fft.irfft(Y, 2 * N)[N:]
        e = d - y

        self.Sxx = 0.7 * self.Sxx + 0.3 * np.sum(np.abs(self.X) ** 2, axis=0)
        if np.sum(x * x) > 1e-7 * N:  # far end active
            Ef = np.fft.rfft(np.concatenate((np.zeros(N), e)))
            Yf = np.fft.rfft(np.concatenate((np.zeros(N), y)))
            pe = np.abs(Ef) ** 2
            py = np.abs(Yf) ** 2
            if np.sum(d * d) > 2.0 * np.sum(e * e):
                self.adapted = min(1.0, self.adapted + 0.02)
            mu_dt = np.minimum(self.mu_max, py / (pe + 1e-10))
            mu = self.adapted * mu_dt + (1 - self.adapted) * self.mu_init
            reg = 1e-2 * float(np.mean(self.Sxx)) + 1e-8
            G = np.conj(self.X) * (mu * Ef / (self.Sxx + reg))[None, :]
            g = np.fft.irfft(G, 2 * N, axis=1)
            g[:, N:] = 0.0  # gradient constraint
            self.W += np.fft.rfft(g, axis=1)
        return e.astype(np.float32)


ENGINES = ("webrtc", "speex", "nlms", "off")


def available_engines() -> list[str]:
    out = []
    try:
        from livekit import rtc  # noqa: F401
        out.append("webrtc")
    except Exception:
        pass
    try:
        import pyaec  # noqa: F401
        out.append("speex")
    except Exception:
        pass
    out += ["nlms", "off"]
    return out


def make_engine(name: str = "auto", *, noise_suppression=True, high_pass_filter=True,
                auto_gain_control=False, filter_ms=300) -> EchoCanceller:
    if name == "auto":
        name = available_engines()[0]
    if name == "webrtc":
        return WebRtcAec(noise_suppression, high_pass_filter, auto_gain_control)
    if name == "speex":
        return SpeexAec(filter_ms=filter_ms, preprocess=noise_suppression)
    if name == "nlms":
        return NlmsAec(filter_ms=filter_ms)
    if name == "off":
        return PassThrough()
    raise ValueError(f"unknown AEC engine {name!r}; choose from {ENGINES}")


class AecProcessor:
    """Frame-level mic pipeline: optional fixed delays + AEC engine.

    ref_delay_ms delays the reference (use when the echo arrives in the mic
    much later than the reference arrives from loopback); mic_delay_ms delays
    the mic (use when the reference arrives *later* than its echo, which no
    causal canceller can handle).  Both default to 0: AEC3 has its own delay
    estimator, the speex/nlms filters cover 0..filter_ms.
    """

    def __init__(self, engine: EchoCanceller, ref_delay_ms: float = 0.0,
                 mic_delay_ms: float = 0.0, output_gain_db: float = 0.0):
        self.engine = engine
        self.ref_delay = DelayLine(int(round(ref_delay_ms * FS / 1000)))
        self.mic_delay = DelayLine(int(round(mic_delay_ms * FS / 1000)))
        self.gain = float(10 ** (output_gain_db / 20))

    def process(self, mic: np.ndarray, ref: np.ndarray) -> np.ndarray:
        assert len(mic) == FRAME and len(ref) == FRAME, "10 ms @ 48 kHz frames"
        out = self.engine.process(self.mic_delay.process(mic), self.ref_delay.process(ref))
        if self.gain != 1.0:
            out = out * self.gain
        return np.clip(out, -1.0, 1.0).astype(np.float32)

    def process_signal(self, mic: np.ndarray, ref: np.ndarray) -> np.ndarray:
        """Offline helper: whole signals, truncated to a multiple of 10 ms."""
        n = (min(len(mic), len(ref)) // FRAME) * FRAME
        out = np.empty(n, dtype=np.float32)
        for s in range(0, n, FRAME):
            out[s:s + FRAME] = self.process(mic[s:s + FRAME], ref[s:s + FRAME])
        return out
