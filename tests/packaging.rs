use std::fs;
use std::path::PathBuf;

fn repo_file(path: &str) -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

#[test]
fn package_provides_a_safe_global_cancel_shortcut() {
    let desktop = repo_file("packaging/applications/io.github.gigatype.cancel.desktop");
    assert!(desktop.contains("Exec=gigatype cancel"));
    assert!(desktop.contains("X-KDE-Shortcuts=Shift+F9"));
}

#[test]
fn stopping_the_service_removes_interrupted_private_audio() {
    let service = repo_file("packaging/systemd/gigatype.service");
    assert!(service.contains("ExecStopPost="));
    assert!(service.contains("gigatype-recording.wav"));
}
