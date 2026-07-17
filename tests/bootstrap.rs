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
    assert!(stdout.contains("ai-sage/GigaAM-v3@e2e_rnnt"));
    assert!(stdout.contains(".local/share/russian-asr/venv"));
    assert!(stdout.contains(".local/share/russian-asr/gigaam-v3-e2e-rnnt"));
    assert!(stdout.contains("tokenizer.model"));
    assert!(
        std::fs::read_dir(home.path()).unwrap().next().is_none(),
        "print-plan must not alter the filesystem"
    );
}

#[test]
fn model_dependencies_include_the_transformers_pyannote_namespace() {
    let requirements = std::fs::read_to_string(format!(
        "{}/requirements-model.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();

    assert!(
        requirements
            .lines()
            .any(|line| line.starts_with("pyannote-core")),
        "Transformers checks the optional pyannote namespace while loading GigaAM"
    );
}
