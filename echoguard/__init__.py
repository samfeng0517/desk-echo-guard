"""desk-echo-guard: system-wide AEC (loopback reference -> VB-Cable) and a
"Hey Siri" anti-trigger filter for call audio on Windows.

The DSP core (everything except :mod:`echoguard.io_backend`) is pure
numpy/scipy (plus optional native AEC engines) and runs on any OS, which is
what makes it testable offline.
"""

__version__ = "0.2.1"

FS = 48000          # internal / device sample rate
FRAME_MS = 10
FRAME = FS * FRAME_MS // 1000  # 480 samples
