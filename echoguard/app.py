"""Real-time runner: threads that connect devices, buffers and DSP.

    [speaker loopback] --ref_buf--+
                                  v
    [physical mic] -----------> AEC ---out_buf--> [CABLE Input]  (Discord/LINE mic = CABLE Output)

    (--siri-guard)
    [CABLE-A Output] --> SiriGuard (~1 s lookahead) --spk_buf--> [speakers]
         ^ Discord/LINE output device = CABLE-A Input

The loopback of the real speakers captures the final speaker signal, i.e.
*after* the Siri guard and including every other app's sound, which is
exactly the echo reference the mic picks up.
"""

from __future__ import annotations

import os
import threading
import time
from dataclasses import dataclass, field

import numpy as np

from . import FS, FRAME, vcable
from .aec import AecProcessor, make_engine
from .buffers import ElasticBuffer
from .delay import estimate_delay_ms
from .io_backend import AudioBackend, thread_done, thread_init
from .metrics import db


@dataclass
class Settings:
    mic: str | None = None            # physical mic (substring); None = default
    speaker: str | None = None        # real speakers (loopback ref + guard output)
    out: str = "CABLE Input"          # virtual mic sink
    engine: str = "auto"              # webrtc | speex | nlms | off | auto
    noise_suppression: bool = True
    high_pass: bool = True
    agc: bool = False
    filter_ms: int = 300              # speex / nlms filter length
    delay_ms: float = 0.0             # extra delay applied to the reference
    mic_delay_ms: float = 0.0         # extra delay applied to the mic
    output_gain_db: float = 0.0
    ref_buffer_ms: float = 20.0       # elastic buffer targets (= added latency)
    out_buffer_ms: float = 40.0
    max_ppm: float = 1000.0
    blocksize: int = FRAME            # device block size request (samples)
    siri_guard: bool = False
    call_in: str = "CABLE-A Output"   # where Discord/LINE output arrives
    siri_mode: str = "always"         # always | sibilant | detect (needs siri_model)
    siri_model: list = field(default_factory=list)
    siri_threshold: float = 0.5
    siri_profile: str = "deep-wide"
    siri_depth_db: float | None = None
    siri_lookahead_s: float = 1.0
    spk_buffer_ms: float = 40.0
    debug_wav: str | None = None      # directory: dump mic/ref/out(/call) wavs
    debug_max_s: float = 600.0
    stats_interval_s: float = 5.0


class _Tap:
    """Bounded recorder for --debug-wav."""

    def __init__(self, max_s):
        self.max = int(max_s * FS)
        self.chunks = []
        self.n = 0

    def add(self, x):
        if self.n < self.max:
            self.chunks.append(np.array(x, np.float32))
            self.n += len(x)

    def array(self):
        return np.concatenate(self.chunks) if self.chunks else np.zeros(0, np.float32)


class EchoGuardApp:
    def __init__(self, settings: Settings, backend: AudioBackend, log=print):
        self.s = settings
        self.backend = backend
        self.log = log
        self.stop_event = threading.Event()
        self.errors: list[BaseException] = []
        self.threads: list[threading.Thread] = []
        ms = FS // 1000
        # a simulated backend may run faster than real time; the buffers'
        # wall-clock interpolation must use the same time scale
        speed = float(getattr(backend, "speed", 1.0))
        clock = (lambda: time.perf_counter() * speed) if speed != 1.0 else time.perf_counter
        self.ref_buf = ElasticBuffer(int(settings.ref_buffer_ms * ms), settings.max_ppm, clock=clock)
        self.out_buf = ElasticBuffer(int(settings.out_buffer_ms * ms), settings.max_ppm, clock=clock)
        self.spk_buf = ElasticBuffer(int(settings.spk_buffer_ms * ms), settings.max_ppm, clock=clock)
        self.aec = None
        self.guard = None
        self.frames = 0
        self.proc_time = 0.0
        self.levels = {"mic": -120.0, "ref": -120.0, "out": -120.0}
        self.mic_peak = 0.0  # since last stats line
        self.taps = {}
        if settings.debug_wav:
            self.taps = {k: _Tap(settings.debug_max_s) for k in ("mic", "ref", "out", "call")}

    # ----------------------------------------------------------------- build
    def build_dsp(self):
        s = self.s
        engine = make_engine(s.engine, noise_suppression=s.noise_suppression,
                             high_pass_filter=s.high_pass, auto_gain_control=s.agc,
                             filter_ms=s.filter_ms)
        self.aec = AecProcessor(engine, ref_delay_ms=s.delay_ms, mic_delay_ms=s.mic_delay_ms,
                                output_gain_db=s.output_gain_db)
        self.log(f"[aec] engine={engine.name} ref_delay={s.delay_ms} ms "
                 f"mic_delay={s.mic_delay_ms} ms")
        if s.siri_guard:
            from .siri_guard import NotchConfig, make_guard
            notch = NotchConfig.profile(s.siri_profile, depth_db=s.siri_depth_db)
            self.guard = make_guard(s.siri_mode, model_paths=s.siri_model,
                                    threshold=s.siri_threshold,
                                    lookahead_s=s.siri_lookahead_s, notch=notch)
            self.log(f"[siri-guard] mode={s.siri_mode} profile={s.siri_profile} "
                     f"bands={notch.bands} depth={notch.depth_db} dB "
                     f"latency={self.guard.latency / FS * 1000:.0f} ms")

    # --------------------------------------------------------------- threads
    def _spawn(self, name, fn, *args):
        def run():
            com = thread_init()
            try:
                fn(*args)
            except BaseException as e:  # propagate to main thread
                self.errors.append(e)
                self.stop_event.set()
            finally:
                thread_done(com)
        t = threading.Thread(target=run, name=name, daemon=True)
        self.threads.append(t)
        t.start()

    def _reader(self, stream, buf, tap=None, process=None):
        n = self.s.blocksize
        while not self.stop_event.is_set():
            x = stream.read(n)
            if process is not None:
                x = process(x)
            if tap is not None:
                tap.add(x)
            buf.push(x)

    def _writer(self, stream, buf):
        n = self.s.blocksize
        while not self.stop_event.is_set():
            stream.write(buf.pop(n))

    def _aec_loop(self, mic):
        acc = np.zeros(0, np.float32)
        while not self.stop_event.is_set():
            x = mic.read(self.s.blocksize)
            acc = np.concatenate((acc, x)) if len(acc) else x
            while len(acc) >= FRAME:
                m, acc = acc[:FRAME], acc[FRAME:]
                r = self.ref_buf.pop(FRAME)
                t0 = time.perf_counter()
                y = self.aec.process(m, r)
                self.proc_time += time.perf_counter() - t0
                self.frames += 1
                self.out_buf.push(y)
                self.mic_peak = max(self.mic_peak, float(np.max(np.abs(m))))
                a = 0.05
                for k, v in (("mic", m), ("ref", r), ("out", y)):
                    self.levels[k] = (1 - a) * self.levels[k] + a * db(float(np.dot(v, v)) / FRAME)
                if self.taps:
                    self.taps["mic"].add(m)
                    self.taps["ref"].add(r)
                    self.taps["out"].add(y)

    # ------------------------------------------------------------------- run
    def start(self):
        s, b = self.s, self.backend
        self.build_dsp()
        try:
            devs = b.list_devices()
        except Exception:
            devs = []
        # "CABLE Input" keeps working after vcable renamed the endpoints
        out_name = vcable.resolve(s.out, "speaker", devs)
        call_name = vcable.resolve(s.call_in, "mic", devs)
        streams = []
        mic = b.open_input(s.mic, blocksize=s.blocksize)
        lb = b.open_input(s.speaker, loopback=True, blocksize=s.blocksize)
        out = b.open_output(out_name, blocksize=s.blocksize)
        streams += [mic, lb, out]
        self._spawn("ref-reader", self._reader, lb, self.ref_buf)
        self._spawn("out-writer", self._writer, out, self.out_buf)
        if self.guard is not None:
            call = b.open_input(call_name, blocksize=s.blocksize)
            spk = b.open_output(s.speaker, blocksize=s.blocksize)
            streams += [call, spk]
            self._spawn("guard-reader", self._reader, call, self.spk_buf,
                        self.taps.get("call"), self.guard.process)
            self._spawn("spk-writer", self._writer, spk, self.spk_buf)
        self._spawn("aec", self._aec_loop, mic)
        self._streams = streams

    def stats_line(self) -> str:
        r, o = self.ref_buf.stats(), self.out_buf.stats()
        per = self.proc_time / max(self.frames, 1) * 1000
        line = (f"mic {self.levels['mic']:6.1f} dB  ref {self.levels['ref']:6.1f} dB  "
                f"out {self.levels['out']:6.1f} dB | ref buf {r['level'] / 48:5.1f} ms "
                f"{r['ppm']:+6.0f} ppm ur={r['underruns']} | out buf {o['level'] / 48:5.1f} ms "
                f"ur={o['underruns']} | aec {per:.2f} ms/10ms")
        if self.guard is not None:
            line += f" | siri detections={self.guard.detections}"
        if self.mic_peak > 0.9:
            line += ("\n  ! mic peaks near full scale - lower the mic gain in Windows; "
                     "AEC3 stops cancelling on saturated input")
        self.mic_peak = 0.0
        return line

    def run(self, duration_s: float | None = None):
        self.start()
        t0 = time.time()
        last = t0
        try:
            while not self.stop_event.is_set():
                time.sleep(0.1)
                now = time.time()
                if self.s.stats_interval_s and now - last >= self.s.stats_interval_s:
                    last = now
                    self.log(self.stats_line())
                if duration_s is not None and now - t0 >= duration_s:
                    break
        except KeyboardInterrupt:
            self.log("stopping ...")
        finally:
            self.stop()
        if self.errors:
            raise self.errors[0]

    def stop(self):
        self.stop_event.set()
        for t in self.threads:
            t.join(timeout=2.0)
        for st in getattr(self, "_streams", []):
            st.close()
        if self.taps:
            self.write_debug()

    def write_debug(self):
        from scipy.io import wavfile
        d = self.s.debug_wav
        os.makedirs(d, exist_ok=True)
        for k, tap in self.taps.items():
            a = tap.array()
            if len(a):
                wavfile.write(os.path.join(d, f"{k}.wav"), FS,
                              np.clip(a * 32767, -32768, 32767).astype(np.int16))
        self.log(f"[debug] wrote wavs to {d}")


def calibrate(settings: Settings, backend: AudioBackend, seconds: float = 4.0,
              level_db: float = -20.0, log=print) -> tuple[float, float]:
    """Play noise bursts on the speakers, record mic and loopback through the
    same buffering the AEC uses, and estimate the ref -> mic delay."""
    s = Settings(**{**settings.__dict__, "engine": "off", "siri_guard": False,
                    "debug_wav": None})
    app = EchoGuardApp(s, backend, log=lambda *a: None)
    mics, refs = [], []
    app.aec = AecProcessor(make_engine("off"))

    rng = np.random.default_rng(0)
    n = int(seconds * FS)
    t = np.arange(n) / FS
    burst = (np.sin(2 * np.pi * 2.0 * t) > 0.3).astype(np.float32)  # on/off pattern
    noise = (rng.standard_normal(n) * 10 ** (level_db / 20) * burst).astype(np.float32)

    def player():
        spk = backend.open_output(settings.speaker, blocksize=s.blocksize)
        try:
            for i in range(0, n, s.blocksize):
                if app.stop_event.is_set():
                    break
                spk.write(noise[i:i + s.blocksize])
        finally:
            spk.close()

    mic = backend.open_input(s.mic, blocksize=s.blocksize)
    lb = backend.open_input(s.speaker, loopback=True, blocksize=s.blocksize)
    app._spawn("ref-reader", app._reader, lb, app.ref_buf)
    time.sleep(0.2)
    app._spawn("player", player)
    total = int((seconds + 0.6) * FS)
    got = 0
    while got < total and not app.stop_event.is_set():
        m = mic.read(FRAME)
        mics.append(m)
        refs.append(app.ref_buf.pop(FRAME))
        got += FRAME
    app.stop_event.set()
    for t_ in app.threads:
        t_.join(timeout=2.0)
    mic.close()
    lb.close()
    if app.errors:
        raise app.errors[0]
    m, r = np.concatenate(mics), np.concatenate(refs)
    if db(float(np.mean(r * r))) < -70:
        log("[calibrate] loopback stayed silent - is --speaker the device that is playing?")
    d, conf = estimate_delay_ms(r, m)
    log(f"[calibrate] ref->mic delay = {d:.1f} ms (confidence {conf:.1f})")
    return d, conf
