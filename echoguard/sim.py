"""Offline / simulated audio backend.

``SimBackend`` serves pre-computed signals as "devices", paced in (scaled)
real time by each reader's own clock, so the threaded app, the elastic
buffers and clock-drift handling can be exercised without any hardware.
Every simulated device can run at its own clock rate (ppm offset), like
real sound cards.
"""

from __future__ import annotations

import threading
import time

import numpy as np

from . import FS
from .io_backend import AudioBackend, DeviceInfo, InputStream, OutputStream


class _Clock:
    """Paces a stream: n samples take n / (FS * (1 + ppm)) / speed seconds."""

    def __init__(self, ppm: float, speed: float):
        self.rate = FS * (1 + ppm * 1e-6) * speed
        self.t0 = None
        self.count = 0

    def wait(self, n):
        if self.t0 is None:
            self.t0 = time.perf_counter()
        self.count += n
        due = self.t0 + self.count / self.rate
        dt = due - time.perf_counter()
        if dt > 0:
            time.sleep(dt)


class SimInput(InputStream):
    def __init__(self, signal: np.ndarray, ppm: float = 0.0, speed: float = 1.0, loop=False):
        self.x = np.asarray(signal, np.float32)
        self.pos = 0
        self.loop = loop
        self.clock = _Clock(ppm, speed)

    def read(self, n):
        self.clock.wait(n)
        if self.pos + n <= len(self.x):
            out = self.x[self.pos:self.pos + n]
        else:
            out = np.zeros(n, np.float32)
            if self.loop and len(self.x):
                idx = (self.pos + np.arange(n)) % len(self.x)
                out = self.x[idx]
            elif self.pos < len(self.x):
                k = len(self.x) - self.pos
                out[:k] = self.x[self.pos:]
        self.pos += n
        return np.array(out, np.float32)


class SimOutput(OutputStream):
    def __init__(self, ppm: float = 0.0, speed: float = 1.0):
        self.clock = _Clock(ppm, speed)
        self.chunks = []
        self.lock = threading.Lock()

    def write(self, x):
        self.clock.wait(len(x))
        with self.lock:
            self.chunks.append(np.array(x, np.float32))

    def data(self):
        with self.lock:
            return np.concatenate(self.chunks) if self.chunks else np.zeros(0, np.float32)


class SimBackend(AudioBackend):
    """inputs: {name: (signal, ppm)} ; loopbacks: {speaker_name: (signal, ppm)} ;
    outputs: {name: ppm}."""

    def __init__(self, inputs=None, loopbacks=None, outputs=None, speed: float = 1.0):
        self.inputs = inputs or {}
        self.loopbacks = loopbacks or {}
        self.output_ppm = outputs or {}
        self.speed = speed
        self.opened_outputs: dict[str, SimOutput] = {}

    def list_devices(self):
        return ([DeviceInfo("mic", n, n, 1) for n in self.inputs] +
                [DeviceInfo("speaker", n, n, 2) for n in self.output_ppm])

    @staticmethod
    def _find(d, name):
        if name is None:
            return next(iter(d))
        for k in d:
            if name.lower() in k.lower():
                return k
        raise LookupError(name)

    def open_input(self, name, loopback=False, blocksize=480):
        d = self.loopbacks if loopback else self.inputs
        sig, ppm = d[self._find(d, name)]
        return SimInput(sig, ppm, self.speed)

    def open_output(self, name, blocksize=480):
        k = self._find(self.output_ppm, name)
        o = SimOutput(self.output_ppm[k], self.speed)
        self.opened_outputs[k] = o
        return o
