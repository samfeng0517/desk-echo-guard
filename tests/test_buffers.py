import numpy as np

from echoguard.buffers import DelayLine, ElasticBuffer
from conftest import report
from echoguard.metrics import best_lag


def test_delay_line_blocks():
    x = np.arange(1000, dtype=np.float32)
    d = DelayLine(37)
    y = np.concatenate([d.process(x[i:i + 100]) for i in range(0, 1000, 100)])
    assert np.all(y[:37] == 0)
    assert np.array_equal(y[37:], x[:-37])


def test_elastic_passthrough_is_transparent_without_drift():
    b = ElasticBuffer(960, clock=lambda: 0.0)
    x = (0.2 * np.random.default_rng(0).standard_normal(48000 * 3)).astype(np.float32)
    out = []
    for k, i in enumerate(range(0, len(x), 480)):
        b.push(x[i:i + 480], now=k * 0.01)
        out.append(b.pop(480, now=k * 0.01 + 0.004))
    y = np.concatenate(out)
    lag = best_lag(x[:96000], y[:96000], 4000)
    seg = slice(48000, 96000)
    err = y[seg] - x[seg.start - lag:seg.stop - lag]
    assert np.max(np.abs(err)) < 1e-3
    assert b.underruns == 0 and abs(b.ratio - 1) < 1e-9


def _drift_run(ppm, seconds=60.0, target=960):
    """Deterministic producer (clock 1+ppm, 480-sample chunks) vs consumer."""
    b = ElasticBuffer(target, clock=lambda: 0.0)
    fs = 48000
    n = int(seconds * fs * (1 + ppm * 1e-6)) + 4800
    x = (0.3 * np.sin(2 * np.pi * 440 * np.arange(n) / (fs * (1 + ppm * 1e-6)))).astype(np.float32)
    t_prod, dtp, pp = 0.0, 480 / (fs * (1 + ppm * 1e-6)), 0
    levels, outs = [], []
    for k in range(int(seconds * 100)):
        t = (k + 1) * 0.01
        while t_prod <= t:
            b.push(x[pp:pp + 480], now=t_prod)
            pp += 480
            t_prod += dtp
        outs.append(b.pop(480, now=t))
        levels.append(b.level)
    return b, np.array(levels), np.concatenate(outs)


def test_elastic_tracks_clock_drift():
    for ppm in (+500.0, -500.0):
        b, levels, y = _drift_run(ppm)
        est = b.stats()["drift_ppm"]
        tail = levels[-1000:]
        report("elastic_drift", true_ppm=ppm, estimated_ppm=float(est),
               level_min_ms=float(tail.min() / 48), level_max_ms=float(tail.max() / 48),
               underruns=b.underruns)
        assert abs(est - ppm) < 30
        assert b.underruns <= 1 and b.resyncs == 0
        assert tail.min() > 0 and tail.max() < 3 * 960
        # output is still a clean 440 Hz tone (spectral purity)
        seg = y[-48000:]
        spec = np.abs(np.fft.rfft(seg * np.hanning(len(seg))))
        k = int(round(440 * len(seg) / 48000))
        tone = np.sum(spec[k - 3:k + 4] ** 2)
        assert 10 * np.log10(tone / (np.sum(spec ** 2) - tone)) > 40


def test_underrun_keeps_time_alignment():
    b = ElasticBuffer(400, clock=lambda: 0.0)
    b.push(np.arange(1000, dtype=np.float32), now=0.0)
    first = b.pop(300, now=1.0)       # continuous level = 1000 -> primed at 400
    assert first[0] == 600 and b.underruns == 0
    b.pop(300, now=1.01)              # only 100 samples left: underrun
    assert b.underruns == 1
    b.push(np.arange(1000, 3000, dtype=np.float32), now=1.02)
    nxt = b.pop(300, now=1.03)
    # the read position kept advancing through the underrun
    assert abs(nxt[0] - (600 + 600)) < 2
