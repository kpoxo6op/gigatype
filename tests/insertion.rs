use std::process::Command;

#[test]
fn insert_command_hands_text_to_the_desktop_injector() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["insert", "прямо в приложение"])
        .env("GIGATYPE_FAKE_INSERT_LOG", temp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.path()).unwrap(),
        "прямо в приложение"
    );
}
