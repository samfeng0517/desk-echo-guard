"""AEC engines on TTS speech (more realistic than the synthetic test
signals): far end = female voice, near end = male voice, room IR + bulk
delay, 40 % far-only / 20 % near-only / 40 % double talk.

    pip install piper-tts pystoi
    python experiments/aec_tts_eval.py --cache .cache
"""

import argparse
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _tts import tts  # noqa: E402
from echoguard.aec import AecProcessor, available_engines, make_engine  # noqa: E402
from echoguard.metrics import align, db, energy, erle_db, stoi  # noqa: E402
from echoguard.signals import make_scenario  # noqa: E402

FAR = ("The quarterly report is due next Friday, so please send me your numbers by Wednesday "
       "afternoon. I also wanted to ask whether anyone has tested the new build on the older "
       "laptops, because last time we saw crashes when the battery saver was turned on. Let's "
       "talk about the schedule for the summer trip after that, since some of us still need to "
       "book flights and hotels before prices go up again.")
NEAR = ("Sure, I can do that. I ran the build on my old machine yesterday and it seemed fine, "
        "but I did not try battery saver mode. For the trip, I was thinking we could leave on "
        "the second weekend of July and stay for about five days. My sister lives near the "
        "coast, so we might be able to stay with her for a couple of nights and save money.")


def norm(x, rms=0.05):
    return (x / np.sqrt(np.mean(x ** 2)) * rms).astype(np.float32)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", default=".cache")
    args = ap.parse_args()
    far = norm(tts(args.cache, "lessac", FAR, "far"))
    near = norm(tts(args.cache, "ryan", NEAR, "near"))
    print("| engine | NS | delay | ERLE far-only | near-only level | near-only STOI | DT STOI out (mic) |")
    print("|---|---|---|---|---|---|---|")
    for delay in (40.0, 150.0):
        sc = make_scenario(dur_s=40.0, delay_ms=delay, far=far, near=near)
        for eng in [e for e in ("webrtc", "speex", "nlms") if e in available_engines()]:
            for ns in (False, True):
                if eng == "nlms" and ns:
                    continue
                out = AecProcessor(make_engine(eng, noise_suppression=ns)).process_signal(sc.mic, sc.ref)
                n = len(out)
                tgt = (sc.mic - sc.echo)[:n]
                nn, dt = sc.near_only[:n], sc.double_talk[:n]
                print(f"| {eng} | {'on' if ns else 'off'} | {delay:.0f} ms | "
                      f"{erle_db(sc.mic, out, sc.far_only[:n], skip_s=3):.1f} dB | "
                      f"{db(energy(out[nn])) - db(energy(tgt[nn])):+.1f} dB | "
                      f"{stoi(*align(tgt[nn], out[nn])):.2f} | "
                      f"{stoi(*align(tgt[dt], out[dt])):.2f} ({stoi(tgt[dt], sc.mic[:n][dt]):.2f}) |")


if __name__ == "__main__":
    main()
