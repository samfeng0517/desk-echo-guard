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
