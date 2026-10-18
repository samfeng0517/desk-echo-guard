import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def report(name: str, **metrics):
    """Print one metrics line (visible with `pytest -s`) and append it to
    $ECHOGUARD_REPORT if set, so measured numbers can be collected."""
    line = name + ": " + ", ".join(
        f"{k}={v:.2f}" if isinstance(v, float) else f"{k}={v}" for k, v in metrics.items())
    print("\n[metrics] " + line)
    path = os.environ.get("ECHOGUARD_REPORT")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
