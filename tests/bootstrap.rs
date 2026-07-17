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
    assert!(stdout.contains("GigaType/gigaam-v3-e2e-rnnt-onnx"));
    assert!(!stdout.contains("venv"));
    assert!(stdout.contains(".local/share/gigatype/models/gigaam-v3-e2e-rnnt"));
    assert!(stdout.contains("v3_e2e_rnnt_encoder.onnx"));
    assert!(stdout.contains("v3_e2e_rnnt_decoder.onnx"));
    assert!(stdout.contains("v3_e2e_rnnt_joint.onnx"));
    assert!(stdout.contains("tokenizer.model"));
    assert!(
        std::fs::read_dir(home.path()).unwrap().next().is_none(),
        "print-plan must not alter the filesystem"
    );
}

#[test]
fn production_tree_has_no_python_model_runtime() {
    assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("requirements-model.txt")
        .exists());
    assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("worker/gigaam_worker.py")
        .exists());
}
