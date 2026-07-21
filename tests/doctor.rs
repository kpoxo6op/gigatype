use std::process::Command;

#[test]
fn doctor_reports_every_runtime_layer() {
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .arg("doctor")
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("Native GigaAM ONNX"));
    assert!(report.contains("Native microphone"));
    assert!(report.contains("Audio output"));
    assert!(report.contains("Clipboard adapter"));
    assert!(report.contains("Auto-insert"));
    assert!(report.contains("Media pause/resume"));
    assert!(report.contains("Session safety"));
}
