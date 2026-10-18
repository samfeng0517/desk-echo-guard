"""Audio device I/O behind a small interface.

Only :class:`SoundcardBackend` touches real hardware (WASAPI on Windows via
the ``soundcard`` package, including loopback capture of a speaker).  The
rest of the program talks to :class:`InputStream` / :class:`OutputStream`,
so it can be driven by :mod:`echoguard.sim` (wav files / synthetic signals)
in tests and on machines without audio devices.

All streams are mono float32 at 48 kHz; multi-channel devices are
down-mixed on input and duplicated on output.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass

import numpy as np

from . import FS


@dataclass
class DeviceInfo:
    kind: str      # "mic" | "speaker" | "loopback"
    name: str
    id: str
    channels: int
    default: bool = False


class InputStream:
    def read(self, n: int) -> np.ndarray:
        """Block until n samples are available; return float32 mono."""
        raise NotImplementedError

    def close(self) -> None:
        pass


class OutputStream:
    def write(self, x: np.ndarray) -> None:
        """Block until x has been queued to the device."""
        raise NotImplementedError

    def close(self) -> None:
        pass


class AudioBackend:
    def list_devices(self) -> list[DeviceInfo]:
        raise NotImplementedError

    def open_input(self, name: str | None, loopback: bool = False,
                   blocksize: int = 480) -> InputStream:
        raise NotImplementedError

    def open_output(self, name: str | None, blocksize: int = 480) -> OutputStream:
        raise NotImplementedError


def thread_init() -> None:
    """Make sure COM is initialised for the calling thread (Windows)."""
    if sys.platform == "win32":
        try:
            import ctypes
            ctypes.windll.ole32.CoInitializeEx(None, 0)  # COINIT_MULTITHREADED
        except Exception:
            pass


def match_device(items, query: str | None, what: str):
    """Pick a device by case-insensitive name substring (or exact id)."""
    if query is None:
        return None
    q = query.lower()
    exact = [d for d in items if d.name.lower() == q or str(d.id) == query]
    if exact:
        return exact[0]
    hits = [d for d in items if q in d.name.lower()]
    if not hits:
        names = "\n  ".join(d.name for d in items)
        raise LookupError(f"no {what} matches {query!r}. Available:\n  {names}")
    if len(hits) > 1:
        # prefer the shortest name ("CABLE Input" over "CABLE Input (VB-Audio ...16ch)")
        hits.sort(key=lambda d: len(d.name))
    return hits[0]


class _SCInput(InputStream):
    def __init__(self, mic, blocksize):
        self._ctx = mic.recorder(samplerate=FS, blocksize=blocksize)
        self._rec = self._ctx.__enter__()

    def read(self, n):
        data = self._rec.record(numframes=n)
        if data.ndim == 2:
            data = data.mean(axis=1)
        return data.astype(np.float32)

    def close(self):
        try:
            self._ctx.__exit__(None, None, None)
        except Exception:
            pass


class _SCOutput(OutputStream):
    def __init__(self, spk, blocksize):
        self.channels = max(1, int(spk.channels))
        self._ctx = spk.player(samplerate=FS, channels=self.channels, blocksize=blocksize)
        self._pl = self._ctx.__enter__()

    def write(self, x):
        x = np.asarray(x, np.float32)
        if self.channels > 1:
            x = np.repeat(x[:, None], self.channels, axis=1)
        self._pl.play(x)

    def close(self):
        try:
            self._ctx.__exit__(None, None, None)
        except Exception:
            pass


class SoundcardBackend(AudioBackend):
    def __init__(self):
        import soundcard as sc  # imported lazily: needs a sound server
        self.sc = sc

    def list_devices(self):
        sc = self.sc
        out = []
        dmic = sc.default_microphone()
        dspk = sc.default_speaker()
        for m in sc.all_microphones(include_loopback=False):
            out.append(DeviceInfo("mic", m.name, str(m.id), m.channels, m.id == dmic.id))
        for s in sc.all_speakers():
            out.append(DeviceInfo("speaker", s.name, str(s.id), s.channels, s.id == dspk.id))
        return out

    def _mic(self, name, loopback):
        sc = self.sc
        if loopback:
            spk = (match_device(sc.all_speakers(), name, "speaker") if name
                   else sc.default_speaker())
            mics = sc.all_microphones(include_loopback=True)
            for m in mics:
                if getattr(m, "isloopback", False) and m.id == spk.id:
                    return m
            return sc.get_microphone(spk.id, include_loopback=True)
        if not name:
            return sc.default_microphone()
        return match_device(sc.all_microphones(include_loopback=False), name, "microphone")

    def open_input(self, name, loopback=False, blocksize=480):
        return _SCInput(self._mic(name, loopback), blocksize)

    def open_output(self, name, blocksize=480):
        sc = self.sc
        spk = match_device(sc.all_speakers(), name, "speaker") if name else sc.default_speaker()
        return _SCOutput(spk, blocksize)

    def resolve_speaker_name(self, name):
        sc = self.sc
        return (match_device(sc.all_speakers(), name, "speaker") if name
                else sc.default_speaker()).name

    def resolve_mic_name(self, name):
        return self._mic(name, False).name
