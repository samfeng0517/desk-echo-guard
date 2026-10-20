import numpy as np

from echoguard.resample import IntegerResampler
from echoguard.metrics import snr_db


def test_block_invariance_and_roundtrip():
    rng = np.random.default_rng(0)
    from scipy import signal
    x = signal.sosfilt(signal.butter(12, 5000, fs=48000, output="sos"),
                       rng.standard_normal(48000)).astype(np.float32)
    d1, u1 = IntegerResampler(3), IntegerResampler(-3)
    y_blocks = np.concatenate([u1.process(d1.process(x[i:i + 480])) for i in range(0, len(x), 480)])
    y_once = IntegerResampler(-3).process(IntegerResampler(3).process(x))
    assert np.allclose(y_blocks, y_once, atol=1e-5)
    # in-band (< 5 kHz) content survives 48k -> 16k -> 48k
    assert snr_db(x[1000:-1000], y_once[1000:-1000], align_max=200) > 30


def test_matches_scipy_lfilter_reference():
    """The polyphase version must equal filtering at the high rate with
    scipy and keeping every k-th sample (down) / zero-stuffing (up)."""
    from scipy import signal
    from echoguard.resample import firwin_lowpass
    assert np.allclose(firwin_lowpass(97, 0.3), signal.firwin(97, 0.3), atol=1e-15)
    x = np.random.default_rng(1).standard_normal(4800).astype(np.float32)
    for f in (3, -3, 2):
        r = IntegerResampler(f)
        sizes = [480, 6, 960, 3] if f > 0 else [160, 1, 7, 333]
        out, i, k = [], 0, 0
        while i < len(x):
            out.append(r.process(x[i:i + sizes[k % len(sizes)]]))
            i += sizes[k % len(sizes)]
            k += 1
        y = np.concatenate(out)
        if f > 0:
            ref = signal.lfilter(r.h, 1.0, x.astype(np.float64))[::f]
        else:
            up = np.zeros(len(x) * -f)
            up[::-f] = x
            ref = signal.lfilter(r.h, 1.0, up)
        assert np.allclose(y, ref, atol=1e-5)
