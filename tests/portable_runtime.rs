use std::process::Command;

#[test]
fn platform_report_exposes_every_portable_adapter() {
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .arg("platform")
        .env("GIGATYPE_FAKE_PLATFORM", "linux-wayland")
        .env_remove("XDG_CURRENT_DESKTOP")
        .output()
        .expect("gigatype should start");
    let report = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(report.contains("platform: linux-wayland"));
    assert!(report.contains("audio: cpal"));
    assert!(report.contains("shortcuts: xdg-desktop-portal"));
    assert!(report.contains("insertion: xdg-desktop-portal"));
    assert!(report.contains("autostart: systemd-user"));
}

#[test]
fn transcribe_file_does_not_need_python() {
    let audio = tempfile::NamedTempFile::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["transcribe-file", audio.path().to_str().unwrap()])
        .env("PATH", "/definitely/no/python/here")
        .env("GIGATYPE_FAKE_TRANSCRIPT", "нативная модель работает")
        .output()
        .expect("gigatype should start");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Нативная модель работает.\n"
    );
}
