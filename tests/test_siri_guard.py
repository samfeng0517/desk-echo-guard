"""Offline checks of the anti-trigger processing (Pipeline B).

We cannot run Apple's detector here, so these tests verify the *signal-level*
claims: the stop bands are really removed (spectral difference), the rest of
the spectrum / overall energy is preserved, intelligibility proxies stay
high, and in detect mode only the phrase region is touched - with the
lookahead making it possible to process the phrase even though the detector
fires at its end.
"""

import numpy as np
import pytest
from scipy import signal

from echoguard.metrics import (align, band_energy_db, db, energy, envelope_correlation,
                               log_spectral_distance, stoi)
from echoguard.siri_guard import (PROFILES, GuardConfig, NotchBank, NotchConfig,
                                  OracleDetector, SibilantDetector, SiriGuard,
                                  design_notch_fir, make_guard)
from echoguard.signals import synth_speech
from conftest import report

FS = 48000


def _gain_db(h, f):
    _, H = signal.freqz(h, worN=[f], fs=FS)
    return 20 * np.log10(abs(H[0]) + 1e-12)


@pytest.mark.parametrize("name", sorted(PROFILES))
def test_notch_response(name):
    cfg = NotchConfig.profile(name)
    h = design_notch_fir(cfg)
    assert len(h) % 2 == 1 and np.allclose(h, h[::-1])  # linear phase
    for lo, hi in cfg.bands:
        for f in np.linspace(lo + 75, hi - 75, 5):  # window smears the very edges
            assert _gain_db(h, f) < -cfg.depth_db + 3, (name, f)
    # pass band untouched (< 0.5 dB) well away from every stop band
    for f in (200, 500, 7500, 10000):
        if all(not (lo - 300 < f < hi + 300) for lo, hi in cfg.bands):
            assert abs(_gain_db(h, f)) < 0.5, (name, f)


def test_streaming_equals_offline():
    x = synth_speech(2.0, seed=4)
    nb1, nb2 = NotchBank(), NotchBank()
    y_blocks = np.concatenate([nb1.process(x[i:i + 480]) for i in range(0, len(x), 480)])
    y_once = nb2.process(x)
    assert np.allclose(y_blocks, y_once, atol=1e-5)


@pytest.mark.parametrize("profile", ["deep-wide", "apple-deep", "formant"])
def test_always_mode_spectral_effect_and_intelligibility(profile):
    x = synth_speech(8.0, seed=5)
    g = make_guard("always", notch=NotchConfig.profile(profile))
    y = g.process_signal(x)
    cfg = g.notch.cfg
    band_drop = [band_energy_db(x, lo, hi) - band_energy_db(y, lo, hi) for lo, hi in cfg.bands]
    total = db(energy(y)) - db(energy(x))
    lsd_all = log_spectral_distance(x, y)
    lsd_outside = log_spectral_distance(x, y, lo=100, hi=min(lo for lo, _ in cfg.bands) - 300)
    st = stoi(x, y)
    env = envelope_correlation(*align(x, y))
    report(f"siri_guard_always[{profile}]", band_attenuation_dB_min=float(min(band_drop)),
           total_energy_change_dB=total, LSD_dB=lsd_all, LSD_outside_bands_dB=lsd_outside,
           STOI=st if st is not None else -1.0, envelope_corr=env)
    assert min(band_drop) > 20.0          # the targeted bands are really gone
    assert lsd_outside < 0.5               # the rest of the spectrum is untouched
    assert total > -3.0                    # most energy is below 3 kHz in speech
    assert env > 0.9
    if st is not None:
        assert st > 0.85


def test_detect_mode_only_touches_phrase_and_uses_lookahead():
    x = synth_speech(6.0, seed=6)
    a, b = int(2.0 * FS), int(2.7 * FS)    # the "Hey Siri" region
    det = OracleDetector([(a, b)])          # fires only once the phrase has ended
    g = SiriGuard(GuardConfig(mode="detect", lookahead_s=1.0), det)
    y = g.process_signal(x)
    R = g.ramp
    diff = np.abs(y - x)
    outside = np.r_[0:a - R - 700, b + R + 700:len(x)]  # FIR half-length margin
    assert np.max(diff[outside]) < 1e-4, "audio outside the phrase must be bit-identical-ish"
    seg_x, seg_y = x[a:b], y[a:b]
    drop = [band_energy_db(seg_x, lo, hi) - band_energy_db(seg_y, lo, hi)
            for lo, hi in g.notch.cfg.bands]
    report("siri_guard_detect[oracle]", phrase_band_attenuation_dB_min=float(min(drop)),
           latency_ms=g.latency / FS * 1000, outside_max_abs_diff=float(np.max(diff[outside])))
    assert min(drop) > 20.0
    assert g.detections == 1


def test_detection_too_late_is_clamped_not_crashing():
    x = synth_speech(3.0, seed=7)
    det = OracleDetector([(int(0.2 * FS), int(1.0 * FS))])
    g = SiriGuard(GuardConfig(mode="detect", lookahead_s=0.2), det)  # lookahead too short
    y = g.process_signal(x)
    assert len(y) == len(x) and np.isfinite(y).all()


def test_sibilant_detector_fires_on_s_like_bursts():
    rng = np.random.default_rng(0)
    x = np.zeros(3 * FS, np.float32)
    sos = signal.butter(4, [4000, 8000], btype="band", fs=FS, output="sos")
    hiss = signal.sosfilt(sos, rng.standard_normal(int(0.12 * FS))).astype(np.float32)
    onsets = [int(0.5 * FS), int(1.5 * FS), int(2.4 * FS)]
    for o in onsets:
        x[o:o + len(hiss)] += 0.1 * hiss / np.std(hiss)
    vowel = synth_speech(3.0, seed=8, pause_prob=0.0)
    vowel_only = vowel.copy()
    det = SibilantDetector()
    regs = []
    for i in range(0, len(x), 480):
        regs += det.feed(x[i:i + 480], i)
    starts = sorted(r[0] for r in regs)
    assert len(starts) == 3
    for o, s in zip(onsets, starts):
        assert abs((s + det.pre) - o) < 0.02 * FS
    # speech with its high band removed (no /s/ energy) must not trigger
    det2 = SibilantDetector()
    lp = signal.butter(10, 2500, fs=FS, output="sos")
    voiced = signal.sosfilt(lp, vowel_only).astype(np.float32)
    n2 = sum(len(det2.feed(voiced[i:i + 480], i)) for i in range(0, len(voiced), 480))
    assert n2 == 0


def test_legacy_apple_profile_maps_to_default():
    assert NotchConfig.profile("apple") == NotchConfig.profile("deep-wide") == NotchConfig()
