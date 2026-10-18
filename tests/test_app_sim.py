"""End-to-end run of the threaded app against simulated devices.

Each simulated device runs on its own clock (ppm offsets), like real sound
cards, at 4x real time.  This exercises the reader/writer threads, elastic
buffers and drift compensation, and the Siri-guard path.
"""

import numpy as np
from scipy import signal

from echoguard.aec import available_engines
from echoguard.app import EchoGuardApp, Settings
from echoguard.metrics import band_energy_db, best_lag, erle_db
from echoguard.signals import make_scenario, synth_speech
from echoguard.sim import SimBackend
from conftest import report

SPEED = 2.0


def test_threaded_pipeline_with_clock_drift():
    engine = "webrtc" if "webrtc" in available_engines() else "nlms"
    ppm = 100.0 if engine == "webrtc" else 0.0
    dur = 16.0
    sc = make_scenario(dur_s=dur, delay_ms=60)
    n = len(sc.ref)
    # the loopback device has its own clock: same sound, more/less samples
    lb = signal.resample(sc.ref, int(round(n * (1 + ppm * 1e-6)))).astype(np.float32)
    call = synth_speech(dur, seed=11)
    be = SimBackend(inputs={"USB Mic": (sc.mic, 0.0), "CABLE-A Output": (call, -40.0)},
                    loopbacks={"Speakers (Realtek)": (lb, ppm)},
                    outputs={"CABLE Input": -60.0, "Speakers (Realtek)": 0.0},
                    speed=SPEED)
    st = Settings(mic="usb", speaker="realtek", out="cable input", engine=engine,
                  noise_suppression=False, stats_interval_s=0,
                  siri_guard=True, siri_mode="always", call_in="cable-a",
                  # thread-scheduling jitter does not shrink with the speed-up
                  ref_buffer_ms=20.0 * SPEED, out_buffer_ms=40.0 * SPEED,
                  spk_buffer_ms=40.0 * SPEED)
    app = EchoGuardApp(st, be, log=lambda *a: None)
    app.run(duration_s=dur / SPEED)

    out = be.opened_outputs["CABLE Input"].data()
    lag = best_lag(sc.near, out[:n], 48000)
    o = out[lag:]
    m = min(len(o), n)
    erle = erle_db(sc.mic[:m], o[:m], sc.far_only[:m], skip_s=3.0)
    rb = app.ref_buf.stats()
    spk = be.opened_outputs["Speakers (Realtek)"].data()
    drop = band_energy_db(call[:len(spk)], 3800, 4200) - band_energy_db(spk, 3800, 4200)
    report(f"threaded_sim[{engine},loopback {ppm:+.0f} ppm]", ERLE_far_only_dB=erle,
           pipeline_latency_ms=lag / 48, ref_drift_estimate_ppm=rb["drift_ppm"],
           ref_underruns=rb["underruns"], guard_4k_band_drop_dB=drop)
    assert erle > (25.0 if engine == "webrtc" else 8.0)
    assert 0 < lag / 48 < 300
    assert drop > 15.0
