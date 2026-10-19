"""PyInstaller entry point for DeskEchoGuard.exe (GUI, no console).

    DeskEchoGuard.exe                      open the settings window
    DeskEchoGuard.exe --self-test out.txt  offline check, report to out.txt
    DeskEchoGuard.exe --vcable rename R    rename the VB-Cable devices (run elevated by the GUI)
"""

import multiprocessing
import os
import sys

if __name__ == "__main__":
    multiprocessing.freeze_support()
    # windowed exe: there is no console, keep libraries that print/tqdm happy
    if sys.stdout is None:
        sys.stdout = open(os.devnull, "w")
    if sys.stderr is None:
        sys.stderr = open(os.devnull, "w")
    if len(sys.argv) > 1 and sys.argv[1] == "--self-test":
        from echoguard.selftest import main
        sys.exit(main(sys.argv[2:]))
    if len(sys.argv) > 1 and sys.argv[1] == "--vcable":
        from echoguard.vcable import main
        sys.exit(main(sys.argv[2:]))
    from echoguard.gui import main
    sys.exit(main(sys.argv[1:]))
