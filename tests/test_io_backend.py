"""soundcard-free checks of the WASAPI output wrapper's pacing."""

import numpy as np

from echoguard import io_backend


class _FakePlayer:
    buffersize = 1056
    currentpadding = 0

    def __init__(self):
        self.played = []

    def play(self, x):
        self.played.append(len(x))


class _Ctx:
    def __init__(self, pl):
        self.pl = pl

    def __enter__(self):
        return self.pl

    def __exit__(self, *a):
        pass


class _FakeSpeaker:
    channels = 2

    def __init__(self):
        self.pl = _FakePlayer()

    def player(self, **kw):
        return _Ctx(self.pl)


def test_output_sleeps_once_until_the_block_fits(monkeypatch):
    slept = []
    monkeypatch.setattr(io_backend.time, "sleep", slept.append)
    spk = _FakeSpeaker()
    out = io_backend._SCOutput(spk, 480)
    spk.pl.currentpadding = 300          # 756 free: fits, no sleep
    out.write(np.zeros(480, np.float32))
    assert slept == []
    spk.pl.currentpadding = 1056 - 120   # 120 free: wait for 360 more samples
    out.write(np.zeros(480, np.float32))
    assert np.allclose(slept, [360 / 48000])
    assert spk.pl.played == [480, 480]
