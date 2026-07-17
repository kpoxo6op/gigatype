use std::process::Command;

#[test]
fn help_describes_the_voice_typing_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .arg("--help")
        .output()
        .expect("gigatype should start");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("toggle"));
    assert!(stdout.contains("cancel"));
    assert!(stdout.contains("doctor"));
    assert!(stdout.contains("microphones"));
}
