use std::fs;
use std::path::PathBuf;
use std::process::Command;

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

#[test]
fn installer_requires_the_complete_v3_checkpoint() {
    let scripts = repo_file("scripts/install.sh") + &repo_file("scripts/bootstrap-model.sh");
    assert!(scripts.contains("v3_e2e_rnnt_encoder.onnx"));
    assert!(scripts.contains("v3_e2e_rnnt_decoder.onnx"));
    assert!(scripts.contains("v3_e2e_rnnt_joint.onnx"));
    assert!(!scripts.contains("requirements-model.txt"));
}

#[test]
fn linux_prefers_the_desktop_audio_server_and_checks_alsa_fallback_headers() {
    let manifest = repo_file("Cargo.toml");
    let installer = repo_file("scripts/install.sh");
    assert!(manifest.contains("features = [\"pulseaudio\"]"));
    assert!(installer.contains("pkg-config --exists alsa"));
}

#[test]
fn release_covers_the_major_unix_package_managers() {
    for path in [
        "packaging/appimage/AppRun",
        "packaging/debian/control",
        "packaging/rpm/gigatype.spec",
        "packaging/homebrew/gigatype.rb",
        "flake.nix",
    ] {
        assert!(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(path)
                .is_file(),
            "missing {path}"
        );
    }
}

#[test]
fn github_hosted_ci_runs_the_real_model_virtual_audio_x11_path() {
    let workflow = repo_file(".github/workflows/ci.yml");
    assert!(workflow.contains("hosted-e2e:"));
    assert!(workflow.contains("runs-on: ubuntu-latest"));
    assert!(workflow.contains("./scripts/ci-hosted-e2e.sh"));
    assert!(!workflow.contains("runs-on: self-hosted"));

    let harness_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/ci-hosted-e2e.sh");
    assert!(harness_path.is_file(), "missing hosted E2E harness");
    let harness = fs::read_to_string(harness_path).unwrap();
    assert!(!harness.contains("~/.cargo/bin/cargo"));
    for boundary in [
        "bootstrap-model.sh",
        "module-null-sink",
        "example.wav",
        "transcribe-file",
        "toggle | grep -qx recording",
        "xclip",
        "editor-output",
    ] {
        assert!(harness.contains(boundary), "E2E omits {boundary}");
    }

    let readme = repo_file("README.md");
    assert!(readme.contains("GitHub-hosted E2E"));
    assert!(readme.contains("does not test a physical microphone or KDE Wayland"));
}

#[test]
fn hosted_e2e_harness_refuses_self_hosted_and_developer_machines() {
    let harness = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/ci-hosted-e2e.sh");
    let output = Command::new("bash")
        .arg(harness)
        .env("GITHUB_ACTIONS", "true")
        .env("RUNNER_ENVIRONMENT", "self-hosted")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("Refusing to run outside a GitHub-hosted runner"));
}
