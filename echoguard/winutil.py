"""Windows-only helpers for the GUI: DPI awareness, crisp window icons and
"start with Windows" (HKCU Run key).  Every function is a no-op elsewhere.
"""

from __future__ import annotations

import os
import sys

RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
RUN_NAME = "DeskEchoGuard"
AUTOSTART_FLAG = "--autostart"
APP_ID = "samfeng0517.DeskEchoGuard"

IS_WIN = sys.platform == "win32"


def enable_dpi_awareness() -> None:
    """Must run before tk.Tk(): Tk reads the screen DPI once at startup, so a
    DPI-unaware process gets 96-DPI fonts that Windows then bitmap-stretches
    (blurry) or a tiny window (when awareness is turned on too late)."""
    if not IS_WIN:
        return
    import ctypes
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(1)  # system DPI aware
    except Exception:
        try:
            ctypes.windll.user32.SetProcessDPIAware()
        except Exception:
            pass
    try:  # own taskbar group + icon instead of python's when run via pythonw
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(APP_ID)
    except Exception:
        pass


def ui_scale(root) -> float:
    """Pixels per 96-DPI pixel (1.0 at 100 %, 2.0 at 200 % / typical 4K)."""
    try:
        return max(1.0, root.winfo_fpixels("1i") / 96.0)
    except Exception:
        return 1.0


def set_window_icon(root, ico_path: str) -> bool:
    """Give the window icons at the exact sizes the current DPI wants (title
    bar = SM_CXSMICON, taskbar / Alt-Tab = SM_CXICON).  Tk's iconbitmap may
    pick a smaller image from the .ico and upscale it."""
    if not IS_WIN:
        return False
    import ctypes
    from ctypes import wintypes
    user32 = ctypes.windll.user32
    user32.LoadImageW.restype = wintypes.HANDLE
    user32.LoadImageW.argtypes = [wintypes.HINSTANCE, wintypes.LPCWSTR, wintypes.UINT,
                                  ctypes.c_int, ctypes.c_int, wintypes.UINT]
    user32.SendMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM,
                                    wintypes.LPARAM]
    IMAGE_ICON, LR_LOADFROMFILE, WM_SETICON = 1, 0x10, 0x80
    root.update_idletasks()
    try:
        hwnd = int(root.wm_frame(), 16)
    except (ValueError, Exception):
        return False
    ok = False
    for which, metric in ((0, 49), (1, 11)):  # ICON_SMALL/SM_CXSMICON, ICON_BIG/SM_CXICON
        size = user32.GetSystemMetrics(metric)
        h = user32.LoadImageW(None, ico_path, IMAGE_ICON, size, size, LR_LOADFROMFILE)
        if h:
            user32.SendMessageW(hwnd, WM_SETICON, which, h)
            ok = True
    return ok


_mutex = None


def claim_single_instance() -> bool:
    """False if another copy is already running (both would fight over the
    same audio devices).  The named mutex lives until this process exits."""
    global _mutex
    if not IS_WIN:
        return True
    import ctypes
    kernel32 = ctypes.windll.kernel32
    kernel32.CreateMutexW.restype = ctypes.c_void_p
    _mutex = kernel32.CreateMutexW(None, False, r"Local\DeskEchoGuard.SingleInstance")
    return kernel32.GetLastError() != 183  # ERROR_ALREADY_EXISTS


# ------------------------------------------------------------ autostart
def autostart_command() -> str:
    """Command line Windows runs at sign-in: open the GUI and start right away."""
    if getattr(sys, "frozen", False):
        return f'"{sys.executable}" {AUTOSTART_FLAG}'
    exe = sys.executable
    pyw = os.path.join(os.path.dirname(exe), "pythonw.exe")
    if os.path.exists(pyw):
        exe = pyw
    # the Run key has no working directory, so put the package on sys.path
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    code = (f"import sys; sys.path.insert(0, r'{root}'); "
            f"from echoguard.gui import main; sys.exit(main(['{AUTOSTART_FLAG}']))")
    return f'"{exe}" -c "{code}"'


def _key(write: bool):
    import winreg
    access = winreg.KEY_READ | (winreg.KEY_SET_VALUE if write else 0)
    return winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, access)


def get_autostart() -> str | None:
    if not IS_WIN:
        return None
    import winreg
    try:
        with _key(False) as k:
            return winreg.QueryValueEx(k, RUN_NAME)[0]
    except OSError:
        return None


def set_autostart(enabled: bool) -> None:
    if not IS_WIN:
        raise OSError("autostart is only supported on Windows")
    import winreg
    with _key(True) as k:
        if enabled:
            winreg.SetValueEx(k, RUN_NAME, 0, winreg.REG_SZ, autostart_command())
        else:
            try:
                winreg.DeleteValue(k, RUN_NAME)
            except FileNotFoundError:
                pass


def refresh_autostart() -> None:
    """Keep the Run entry pointing at this copy if the exe was moved/updated."""
    cur = get_autostart()
    if cur is not None and cur != autostart_command():
        try:
            set_autostart(True)
        except OSError:
            pass
