import numpy as np

from echoguard.delay import estimate_delay_ms
from echoguard.signals import make_scenario


def test_gcc_phat_finds_echo_delay():
    for d in (25.0, 120.0):
        sc = make_scenario(dur_s=10, delay_ms=d)
        est, conf = estimate_delay_ms(sc.ref[:int(4 * 48000)], sc.mic[:int(4 * 48000)])
        # IR direct path is at +3 ms
        assert abs(est - (d + 3.0)) < 1.0, est
        assert conf > 5


def test_negative_delay_detected():
    sc = make_scenario(dur_s=6, delay_ms=0)
    late_ref = np.concatenate((np.zeros(2400, np.float32), sc.ref))[:len(sc.ref)]
    est, _ = estimate_delay_ms(late_ref[:4 * 48000], sc.mic[:4 * 48000])
    assert abs(est - (3.0 - 50.0)) < 1.0
