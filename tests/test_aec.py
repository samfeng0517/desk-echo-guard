"""Offline AEC verification on synthetic signals.

mic = near-end speech + (reference delayed and filtered by a room IR) + noise.
Layout: 40 % far-end only, 20 % near-end only, 40 % double talk.
"""

import numpy as np
import pytest

from echoguard.aec import AecProcessor, available_engines, make_engine
from echoguard.metrics import db, energy, envelope_correlation, align, erle_db, stoi
from echoguard.signals import make_scenario
from conftest import report

ENGINES = [e for e in ("webrtc", "speex", "nlms") if e in available_engines()]
# minimum far-end-only ERLE expected per engine (dB)
MIN_ERLE = {"webrtc": 30.0, "speex": 15.0, "nlms": 10.0}


@pytest.fixture(scope="module", params=[40.0, 150.0], ids=["delay40ms", "delay150ms"])
def scenario(request):
    return request.param, make_scenario(dur_s=30.0, delay_ms=request.param, seed=3)


def _intelligibility(clean, test):
    c, t = align(clean, test)
    s = stoi(c, t)
    return s if s is not None else envelope_correlation(c, t)


@pytest.mark.parametrize("engine", ENGINES)
@pytest.mark.parametrize("ns", [False, True], ids=["ns_off", "ns_on"])
def test_aec_engine(engine, ns, scenario):
    if engine == "nlms" and ns:
        pytest.skip("nlms has no noise suppressor")
    delay, sc = scenario
    proc = AecProcessor(make_engine(engine, noise_suppression=ns, filter_ms=300))
    out = proc.process_signal(sc.mic, sc.ref)
    n = len(out)
    mic, near_target = sc.mic[:n], (sc.mic - sc.echo)[:n]  # what a perfect AEC outputs

    erle = erle_db(mic, out, sc.far_only[:n], skip_s=3.0)
    nn, dt = sc.near_only[:n], sc.double_talk[:n]
    near_level = db(energy(out[nn])) - db(energy(near_target[nn]))
    near_intel = _intelligibility(near_target[nn], out[nn])
    dt_intel_out = _intelligibility(near_target[dt], out[dt])
    dt_intel_mic = _intelligibility(near_target[dt], mic[dt])
    report(f"aec[{engine},ns={int(ns)},delay={delay:.0f}ms]", ERLE_far_only_dB=erle,
           near_only_level_change_dB=near_level, near_only_STOI=near_intel,
           double_talk_STOI_out=dt_intel_out, double_talk_STOI_mic=dt_intel_mic)

    assert erle >= MIN_ERLE[engine]
    # near end alone must come through: level within 3 dB, intelligibility kept
    assert abs(near_level) < 3.0
    assert near_intel > 0.85
    # double talk: output must not be worse than doing nothing by a wide margin
    assert dt_intel_out > dt_intel_mic - 0.15


def test_passthrough_has_no_erle():
    sc = make_scenario(dur_s=12.0, delay_ms=40)
    out = AecProcessor(make_engine("off")).process_signal(sc.mic, sc.ref)
    assert abs(erle_db(sc.mic, out, sc.far_only[:len(out)])) < 0.01


def test_ref_delay_line_helps_short_filter():
    """A 600 ms bulk delay is outside a 300 ms nlms filter; --delay-ms fixes it."""
    sc = make_scenario(dur_s=20.0, delay_ms=600)
    plain = AecProcessor(make_engine("nlms", filter_ms=300)).process_signal(sc.mic, sc.ref)
    fixed = AecProcessor(make_engine("nlms", filter_ms=300), ref_delay_ms=590).process_signal(sc.mic, sc.ref)
    e_plain = erle_db(sc.mic, plain, sc.far_only[:len(plain)], skip_s=3)
    e_fixed = erle_db(sc.mic, fixed, sc.far_only[:len(fixed)], skip_s=3)
    report("delay_compensation[nlms,600ms]", ERLE_without=e_plain, ERLE_with_delay_ms=e_fixed)
    assert e_plain < 3.0 and e_fixed > 10.0
