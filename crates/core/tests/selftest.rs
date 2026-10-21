//! `--self-test` passes on this build and writes its report.

use echoguard_core::selftest;

#[test]
fn self_test_passes_and_writes_report() {
    let report = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("selftest.txt");
    let code = selftest::main(Some(&report));
    let text = std::fs::read_to_string(&report).unwrap();
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("ok   AEC webrtc: ERLE"), "{text}");
    assert!(text.contains("ok   AEC speex: ERLE"), "{text}");
    assert!(text.contains("ok   AEC nlms: ERLE"), "{text}");
    assert!(text.contains("siri notch 4 kHz"), "{text}");
    // no audio API on Linux CI: informational only
    assert!(text.contains("info audio"), "{text}");
    assert!(text.trim_end().ends_with("RESULT: PASS"), "{text}");
}
