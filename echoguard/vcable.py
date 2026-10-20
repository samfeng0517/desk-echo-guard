"""Find the VB-Cable endpoints by role, and give them names that say what
they are for.

VB-Cable's own names ("CABLE Input" is a speaker, "CABLE Output" is a mic)
are confusing in Discord / LINE device lists.  Windows lets an administrator
rename an endpoint (Sound control panel -> Properties); that changes the
endpoint's PKEY_Device_DeviceDesc, so the name everyone sees becomes
"<new name> (VB-Audio Virtual Cable)".  VB-Cable endpoints this program
does not use (CABLE In 16ch, CABLE-B ...) can also be disabled, as the Sound
control panel's "Disable" does, so they drop out of every device list.

Both need administrator rights, so the GUI starts this module as a separate
process via UAC (:func:`run_elevated`) with comma-separated actions:

    python -m echoguard.vcable rename,hide RESULT.json
    python -m echoguard.vcable restore     RESULT.json   (names back + re-enable)
"""

from __future__ import annotations

import json
import os
import sys
from dataclasses import dataclass


@dataclass(frozen=True)
class Role:
    key: str
    kind: str        # "speaker" (render) | "mic" (capture), as DeviceInfo.kind
    original: str    # VB-Audio's endpoint description
    renamed: str     # what we rename it to


ROLES = (
    Role("out", "speaker", "CABLE Input", "EchoGuard 內部輸出（勿選）"),
    Role("virtual_mic", "mic", "CABLE Output", "回聲消除麥克風"),
    Role("call_speaker", "speaker", "CABLE-A Input", "Siri防護喇叭"),
    Role("call_in", "mic", "CABLE-A Output", "EchoGuard 內部輸入（勿選）"),
)
BY_KEY = {r.key: r for r in ROLES}
DOWNLOAD_URL = "https://vb-audio.com/Cable/"


def _desc_is(name: str, desc: str) -> bool:
    """True if a friendly name "<desc> (<interface>)" (or bare "<desc>") has
    exactly this description; "CABLE Input" must not match "CABLE In 16ch"."""
    n, d = name.strip().lower(), desc.lower()
    return n == d or n.startswith(d + " (")


def role_of(name: str | None, kind: str | None = None) -> Role | None:
    """The role a device name (original or renamed, with or without the
    "(interface)" suffix) stands for."""
    if not name:
        return None
    for r in ROLES:
        if (kind is None or r.kind == kind) and (_desc_is(name, r.original)
                                                 or _desc_is(name, r.renamed)):
            return r
    return None


def find(devices, role: Role) -> str | None:
    """Name of the present device filling this role, either name style."""
    for d in devices:
        if d.kind == role.kind and role_of(d.name, role.kind) is role:
            return d.name
    return None


def resolve(name: str | None, kind: str, devices) -> str | None:
    """Map a configured VB-Cable name to the device that is there now, so
    "CABLE Input" still works after the rename (and the other way round).
    Any other name is returned unchanged."""
    r = role_of(name, kind)
    if r is None:
        return name
    return find(devices, r) or name


def is_vbcable(name: str) -> bool:
    return "vb-audio" in name.lower()


def status(devices) -> dict:
    """{"found": VB-Cable present, "renamed": every present role renamed,
    "names": {role key: current device name},
    "unused": enabled VB-Cable devices this program has no use for}"""
    names = {r.key: find(devices, r) for r in ROLES}
    present = [r for r in ROLES if names[r.key]]
    renamed = bool(present) and all(_desc_is(names[r.key], r.renamed) for r in present)
    unused = [d.name for d in devices if is_vbcable(d.name) and role_of(d.name, d.kind) is None]
    return {"found": names["out"] is not None and names["virtual_mic"] is not None,
            "renamed": renamed, "names": names, "unused": unused}


def display_name(devices, key: str) -> str:
    """Short name to tell the user to pick in Discord / LINE."""
    r = BY_KEY[key]
    name = find(devices, r)
    return r.renamed if name and _desc_is(name, r.renamed) else r.original


# --------------------------------------------------------------- Windows
_CLSID_MMDeviceEnumerator = "{BCDE0395-E52F-467C-8E3D-C4579291692E}"
_IID_IMMDeviceEnumerator = "{A95664D2-9614-4F35-A746-DE8DB63617E6}"
_PKEY_FMTID = "{A45C254E-DF1C-4EFD-8020-67D146A850E0}"
_PID_DEVICE_DESC, _PID_FRIENDLY_NAME = 2, 14
_VT_LPWSTR = 31
# IPolicyConfig is undocumented but stable since Windows 7; it is what the
# Sound control panel uses to disable / enable an endpoint
_CLSID_PolicyConfigClient = "{870AF99C-171D-4F9E-AF0D-E63DF40C2BC9}"
_IID_IPolicyConfig = "{F8679F50-850A-41CF-9C72-430F290290C8}"
STATE_ACTIVE, STATE_DISABLED = 1, 2


def _com():
    """ctypes plumbing for IMMDeviceEnumerator / IPropertyStore (no comtypes)."""
    import ctypes
    from ctypes import wintypes

    class GUID(ctypes.Structure):
        _fields_ = [("d1", wintypes.DWORD), ("d2", wintypes.WORD), ("d3", wintypes.WORD),
                    ("d4", ctypes.c_ubyte * 8)]

    class PROPERTYKEY(ctypes.Structure):
        _fields_ = [("fmtid", GUID), ("pid", wintypes.DWORD)]

    class PROPVARIANT(ctypes.Structure):
        _fields_ = [("vt", wintypes.USHORT), ("r1", wintypes.WORD), ("r2", wintypes.WORD),
                    ("r3", wintypes.WORD), ("ptr", ctypes.c_void_p),
                    ("pad", ctypes.c_void_p)]

    ole32 = ctypes.windll.ole32

    def guid(s):
        g = GUID()
        hr = ole32.CLSIDFromString(ctypes.c_wchar_p(s), ctypes.byref(g))
        if hr:
            raise OSError(f"CLSIDFromString failed 0x{hr & 0xFFFFFFFF:08X}")
        return g

    def call(obj, index, *args, argtypes=()):
        """Call vtable slot `index` of COM pointer `obj`; raise on failure."""
        vtbl = ctypes.cast(obj, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        fn = ctypes.WINFUNCTYPE(ctypes.HRESULT, ctypes.c_void_p, *argtypes)(vtbl[index])
        return fn(obj, *args)

    return ctypes, wintypes, ole32, GUID, PROPERTYKEY, PROPVARIANT, guid, call


def list_endpoints() -> list[dict]:
    """Active and disabled render + capture endpoints: id, kind, state,
    desc, friendly name."""
    ctypes, wintypes, ole32, _, PROPERTYKEY, PROPVARIANT, guid, call = _com()
    ole32.CoInitializeEx(None, 0)
    enum = ctypes.c_void_p()
    hr = ole32.CoCreateInstance(ctypes.byref(guid(_CLSID_MMDeviceEnumerator)), None, 1,
                                ctypes.byref(guid(_IID_IMMDeviceEnumerator)),
                                ctypes.byref(enum))
    if hr:
        raise OSError(f"CoCreateInstance(MMDeviceEnumerator) failed 0x{hr & 0xFFFFFFFF:08X}")
    P = ctypes.c_void_p
    coll = P()
    # IMMDeviceEnumerator::EnumAudioEndpoints(eAll, ACTIVE | DISABLED)
    call(enum, 3, 2, STATE_ACTIVE | STATE_DISABLED, ctypes.byref(coll),
         argtypes=(ctypes.c_int, wintypes.DWORD, ctypes.POINTER(P)))
    count = wintypes.UINT()
    call(coll, 3, ctypes.byref(count), argtypes=(ctypes.POINTER(wintypes.UINT),))
    out = []
    for i in range(count.value):
        dev = P()
        call(coll, 4, i, ctypes.byref(dev), argtypes=(wintypes.UINT, ctypes.POINTER(P)))
        pid = ctypes.c_wchar_p()
        call(dev, 5, ctypes.byref(pid), argtypes=(ctypes.POINTER(ctypes.c_wchar_p),))
        dev_id = pid.value
        ole32.CoTaskMemFree(pid)
        state = wintypes.DWORD()
        call(dev, 6, ctypes.byref(state), argtypes=(ctypes.POINTER(wintypes.DWORD),))
        store = P()
        call(dev, 4, 0, ctypes.byref(store),  # OpenPropertyStore(STGM_READ)
             argtypes=(wintypes.DWORD, ctypes.POINTER(P)))
        props = {}
        for p in (_PID_DEVICE_DESC, _PID_FRIENDLY_NAME):
            key = PROPERTYKEY(guid(_PKEY_FMTID), p)
            pv = PROPVARIANT()
            call(store, 5, ctypes.byref(key), ctypes.byref(pv),
                 argtypes=(ctypes.POINTER(PROPERTYKEY), ctypes.POINTER(PROPVARIANT)))
            props[p] = ctypes.wstring_at(pv.ptr) if pv.vt == _VT_LPWSTR and pv.ptr else ""
            ole32.PropVariantClear(ctypes.byref(pv))
        call(store, 2)
        call(dev, 2)
        # endpoint ids: "{0.0.0.00000000}.{guid}" render, "{0.0.1.00000000}.{guid}" capture
        kind = "mic" if dev_id.startswith("{0.0.1.") else "speaker"
        out.append({"id": dev_id, "kind": kind, "state": state.value,
                    "desc": props[_PID_DEVICE_DESC],
                    "name": props[_PID_FRIENDLY_NAME]})
    call(coll, 2)
    call(enum, 2)
    return out


def set_description(dev_id: str, desc: str) -> None:
    """Rename one endpoint (needs administrator rights)."""
    ctypes, wintypes, ole32, _, PROPERTYKEY, PROPVARIANT, guid, call = _com()
    ole32.CoInitializeEx(None, 0)
    P = ctypes.c_void_p
    enum, dev, store = P(), P(), P()
    hr = ole32.CoCreateInstance(ctypes.byref(guid(_CLSID_MMDeviceEnumerator)), None, 1,
                                ctypes.byref(guid(_IID_IMMDeviceEnumerator)),
                                ctypes.byref(enum))
    if hr:
        raise OSError(f"CoCreateInstance(MMDeviceEnumerator) failed 0x{hr & 0xFFFFFFFF:08X}")
    try:
        call(enum, 5, dev_id, ctypes.byref(dev),  # GetDevice
             argtypes=(ctypes.c_wchar_p, ctypes.POINTER(P)))
        call(dev, 4, 2, ctypes.byref(store),  # OpenPropertyStore(STGM_READWRITE)
             argtypes=(wintypes.DWORD, ctypes.POINTER(P)))
        buf = ctypes.create_unicode_buffer(desc)
        pv = PROPVARIANT()
        pv.vt, pv.ptr = _VT_LPWSTR, ctypes.cast(buf, ctypes.c_void_p).value
        key = PROPERTYKEY(guid(_PKEY_FMTID), _PID_DEVICE_DESC)
        call(store, 6, ctypes.byref(key), ctypes.byref(pv),  # SetValue
             argtypes=(ctypes.POINTER(PROPERTYKEY), ctypes.POINTER(PROPVARIANT)))
        call(store, 7)  # Commit
    finally:
        for o in (store, dev, enum):
            if o:
                call(o, 2)


def set_enabled(dev_id: str, enabled: bool) -> None:
    """Enable / disable one endpoint (needs administrator rights)."""
    ctypes, _, ole32, _, _, _, guid, call = _com()
    ole32.CoInitializeEx(None, 0)
    pc = ctypes.c_void_p()
    hr = ole32.CoCreateInstance(ctypes.byref(guid(_CLSID_PolicyConfigClient)), None, 1,
                                ctypes.byref(guid(_IID_IPolicyConfig)), ctypes.byref(pc))
    if hr:
        raise OSError(f"CoCreateInstance(PolicyConfig) failed 0x{hr & 0xFFFFFFFF:08X}")
    try:  # IPolicyConfig::SetEndpointVisibility
        call(pc, 14, dev_id, int(enabled), argtypes=(ctypes.c_wchar_p, ctypes.c_int))
    finally:
        call(pc, 2)


def plan(endpoints, restore: bool = False) -> list[tuple[dict, str]]:
    """(endpoint, new description) for every VB-Cable endpoint to change."""
    todo = []
    for ep in endpoints:
        if not is_vbcable(ep["name"]):
            continue  # never touch anything that is not VB-Cable
        for r in ROLES:
            src, dst = (r.renamed, r.original) if restore else (r.original, r.renamed)
            if ep["kind"] == r.kind and ep["desc"].strip().lower() == src.lower():
                todo.append((ep, dst))
    return todo


def plan_visibility(endpoints, show: bool = False) -> list[dict]:
    """VB-Cable endpoints without a role to disable (or, with show, to
    enable again).  The ones this program uses are never disabled."""
    want = STATE_DISABLED if show else STATE_ACTIVE
    return [ep for ep in endpoints
            if is_vbcable(ep["name"]) and ep.get("state", STATE_ACTIVE) == want
            and role_of(ep["desc"], ep["kind"]) is None]


ACTIONS = ("rename", "hide", "restore")


def apply(actions) -> list[str]:
    """Run rename / hide / restore (= names back + enable again) in order."""
    done = []
    for action in actions:
        if action not in ACTIONS:
            raise ValueError(f"unknown action {action!r}")
        eps = list_endpoints()
        if action in ("rename", "restore"):
            for ep, new in plan(eps, restore=action == "restore"):
                set_description(ep["id"], new)
                done.append(f"{ep['desc']} → {new}")
        if action in ("hide", "restore"):
            show = action == "restore"
            for ep in plan_visibility(eps, show=show):
                set_enabled(ep["id"], show)
                done.append(f"{'啟用' if show else '停用'} {ep['desc']}")
    return done



def run_elevated(action: str, timeout_s: float = 120.0) -> tuple[bool, str]:
    """Run actions ("rename,hide", "restore", ...) in an elevated copy of
    this program (UAC prompt) and wait for it.  Returns (ok, message)."""
    import ctypes
    import tempfile
    from ctypes import wintypes

    fd, result = tempfile.mkstemp(prefix="echoguard-vcable-", suffix=".json")
    os.close(fd)
    os.remove(result)
    if getattr(sys, "frozen", False):
        exe, params = sys.executable, f'--vcable {action} "{result}"'
    else:
        exe = sys.executable
        pyw = os.path.join(os.path.dirname(exe), "pythonw.exe")
        if os.path.exists(pyw):
            exe = pyw
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        code = (f"import sys; sys.path.insert(0, r'{root}'); "
                f"from echoguard.vcable import main; sys.exit(main(sys.argv[1:]))")
        params = f'-c "{code}" {action} "{result}"'

    class SHELLEXECUTEINFOW(ctypes.Structure):
        _fields_ = [("cbSize", wintypes.DWORD), ("fMask", ctypes.c_ulong),
                    ("hwnd", wintypes.HWND), ("lpVerb", wintypes.LPCWSTR),
                    ("lpFile", wintypes.LPCWSTR), ("lpParameters", wintypes.LPCWSTR),
                    ("lpDirectory", wintypes.LPCWSTR), ("nShow", ctypes.c_int),
                    ("hInstApp", wintypes.HINSTANCE), ("lpIDList", ctypes.c_void_p),
                    ("lpClass", wintypes.LPCWSTR), ("hkeyClass", wintypes.HKEY),
                    ("dwHotKey", wintypes.DWORD), ("hIcon", wintypes.HANDLE),
                    ("hProcess", wintypes.HANDLE)]

    sei = SHELLEXECUTEINFOW()
    sei.cbSize = ctypes.sizeof(sei)
    sei.fMask = 0x40  # SEE_MASK_NOCLOSEPROCESS
    sei.lpVerb, sei.lpFile, sei.lpParameters, sei.nShow = "runas", exe, params, 0
    if not ctypes.windll.shell32.ShellExecuteExW(ctypes.byref(sei)):
        return False, "已取消（需要在 Windows 的確認視窗按「是」）"
    k32 = ctypes.windll.kernel32
    k32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
    k32.CloseHandle.argtypes = (wintypes.HANDLE,)
    k32.WaitForSingleObject(sei.hProcess, int(timeout_s * 1000))
    k32.CloseHandle(sei.hProcess)
    try:
        with open(result, encoding="utf-8") as f:
            r = json.load(f)
        os.remove(result)
    except (OSError, ValueError):
        return False, "改名程式沒有回報結果"
    if r.get("error"):
        return False, r["error"]
    done = r.get("done", [])
    return True, ("、".join(done) if done else "沒有需要變更的裝置")


def main(argv=None) -> int:
    """Elevated entry: `rename,hide|restore RESULT.json`."""
    argv = sys.argv[1:] if argv is None else argv
    actions = argv[0].split(",") if argv else []
    if len(argv) != 2 or not actions or any(a not in ACTIONS for a in actions):
        print("usage: python -m echoguard.vcable rename[,hide]|hide|restore RESULT.json")
        return 2
    result = argv[1]
    try:
        r = {"done": apply(actions)}
    except Exception as e:
        r = {"error": f"{type(e).__name__}: {e}"}
    with open(result, "w", encoding="utf-8") as f:
        json.dump(r, f, ensure_ascii=False)
    return 1 if "error" in r else 0


if __name__ == "__main__":
    sys.exit(main())
