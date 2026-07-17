use std::process::Command;

#[test]
fn transcribe_file_uses_the_native_model_protocol() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["transcribe-file", temp.path().to_str().unwrap()])
        .env("GIGATYPE_FAKE_TRANSCRIPT", "это работает")
        .output()
        .expect("gigatype should start");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Это работает.\n");
}
