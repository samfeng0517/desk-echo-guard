import numpy as np

from echoguard import vcable
from echoguard.app import EchoGuardApp, Settings
from echoguard.io_backend import DeviceInfo
from echoguard.signals import make_scenario
from echoguard.sim import SimBackend

ORIGINAL = [DeviceInfo("speaker", "CABLE Input (VB-Audio Virtual Cable)", "1", 2),
            DeviceInfo("speaker", "CABLE In 16ch (VB-Audio Virtual Cable)", "2", 16),
            DeviceInfo("speaker", "CABLE-A Input (VB-Audio Cable A)", "3", 2),
            DeviceInfo("mic", "CABLE Output (VB-Audio Virtual Cable)", "4", 2),
            DeviceInfo("mic", "CABLE-A Output (VB-Audio Cable A)", "5", 2),
            DeviceInfo("speaker", "Speakers (Realtek(R) Audio)", "6", 2)]
RENAMED = [DeviceInfo(d.kind, d.name.replace(r.original, r.renamed, 1), d.id, d.channels)
           if (r := vcable.role_of(d.name, d.kind)) else d for d in ORIGINAL]


def test_role_matching_is_exact_on_description():
    assert vcable.role_of("CABLE Input (VB-Audio Virtual Cable)").key == "out"
    assert vcable.role_of("CABLE Input").key == "out"
    assert vcable.role_of("CABLE In 16ch (VB-Audio Virtual Cable)") is None
    assert vcable.role_of("CABLE-A Input (VB-Audio Cable A)").key == "call_speaker"
    assert vcable.role_of("回聲消除麥克風 (VB-Audio Virtual Cable)").key == "virtual_mic"
    assert vcable.role_of("CABLE Input", kind="mic") is None


def test_resolve_follows_rename_both_ways():
    assert vcable.resolve("CABLE Input", "speaker", RENAMED) == \
        "EchoGuard 內部輸出（勿選） (VB-Audio Virtual Cable)"
    assert vcable.resolve("CABLE-A Output (VB-Audio Cable A)", "mic", RENAMED) == \
        "EchoGuard 內部輸入（勿選） (VB-Audio Cable A)"
    assert vcable.resolve("EchoGuard 內部輸出（勿選）", "speaker", ORIGINAL) == \
        "CABLE Input (VB-Audio Virtual Cable)"
    assert vcable.resolve("Speakers", "speaker", RENAMED) == "Speakers"
    assert vcable.resolve("CABLE Input", "speaker", []) == "CABLE Input"


def test_status_and_display_names():
    s = vcable.status(ORIGINAL)
    assert s["found"] and not s["renamed"]
    assert vcable.display_name(ORIGINAL, "virtual_mic") == "CABLE Output"
    s = vcable.status(RENAMED)
    assert s["found"] and s["renamed"]
    assert vcable.display_name(RENAMED, "virtual_mic") == "回聲消除麥克風"
    assert vcable.display_name(RENAMED, "call_speaker") == "Siri防護喇叭"
    assert not vcable.status(ORIGINAL[-1:])["found"]


def test_plan_only_touches_vb_audio_endpoints():
    eps = [{"id": "a", "kind": "speaker", "desc": "CABLE Input",
            "name": "CABLE Input (VB-Audio Virtual Cable)"},
           {"id": "b", "kind": "speaker", "desc": "CABLE In 16ch",
            "name": "CABLE In 16ch (VB-Audio Virtual Cable)"},
           {"id": "c", "kind": "mic", "desc": "CABLE Output",
            "name": "CABLE Output (VB-Audio Virtual Cable)"},
           {"id": "d", "kind": "speaker", "desc": "CABLE Input",
            "name": "CABLE Input (Some Other Driver)"}]
    todo = {ep["id"]: new for ep, new in vcable.plan(eps)}
    assert todo == {"a": "EchoGuard 內部輸出（勿選）", "c": "回聲消除麥克風"}
    renamed = [dict(ep, desc=todo.get(ep["id"], ep["desc"])) for ep in eps]
    assert vcable.plan(renamed) == []
    back = {ep["id"]: new for ep, new in vcable.plan(renamed, restore=True)}
    assert back == {"a": "CABLE Input", "c": "CABLE Output"}


def test_app_finds_renamed_cable_with_default_settings():
    sc = make_scenario(dur_s=3, delay_ms=40)
    sink = "EchoGuard 內部輸出（勿選） (VB-Audio Virtual Cable)"
    be = SimBackend(inputs={"USB Mic": (sc.mic, 0.0)},
                    loopbacks={"Speakers": (sc.ref, 0.0)},
                    outputs={sink: 0.0, "Speakers": 0.0}, speed=4.0)
    app = EchoGuardApp(Settings(engine="nlms", stats_interval_s=0), be, log=lambda *a: None)
    app.run(duration_s=0.5)
    assert len(be.opened_outputs[sink].data()) > 0
    assert np.isfinite(be.opened_outputs[sink].data()).all()
