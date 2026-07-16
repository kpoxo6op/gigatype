use std::process::Command;

#[test]
fn bootstrap_can_explain_the_install_without_downloading() {
    let home = tempfile::tempdir().unwrap();
    let script = format!("{}/scripts/bootstrap-model.sh", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .args([script.as_str(), "--print-plan"])
        .env("HOME", home.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("ai-sage/GigaAM-Multilingual@ctc"));
    assert!(stdout.contains(".local/share/russian-asr/venv"));
    assert!(stdout.contains(".local/share/russian-asr/gigaam-multilingual-ctc"));
    assert!(
        std::fs::read_dir(home.path()).unwrap().next().is_none(),
        "print-plan must not alter the filesystem"
    );
}
