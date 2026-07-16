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

#[test]
fn insert_preserves_every_clipboard_mime_type() {
    let temp = tempfile::tempdir().unwrap();
    let insert_log = temp.path().join("insert.log");
    let clipboard_log = temp.path().join("clipboard.log");

    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["insert", "текст"])
        .env("GIGATYPE_FAKE_INSERT_LOG", &insert_log)
        .env("GIGATYPE_FAKE_CLIPBOARD_LOG", &clipboard_log)
        .env(
            "GIGATYPE_FAKE_CLIPBOARD_MIMES",
            "text/plain,image/png,text/uri-list",
        )
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(clipboard_log).unwrap(),
        concat!(
            "capture:text/plain,image/png,text/uri-list\n",
            "set:text/plain;charset=utf-8\n",
            "wait:120ms\n",
            "restore:text/plain,image/png,text/uri-list\n",
        )
    );
}

#[test]
fn insertion_delay_and_terminal_paste_chord_are_configurable() {
    let temp = tempfile::tempdir().unwrap();
    let clipboard_log = temp.path().join("clipboard.log");
    let key_log = temp.path().join("keys.log");

    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["insert", "терминал"])
        .env("GIGATYPE_FAKE_CLIPBOARD_LOG", &clipboard_log)
        .env("GIGATYPE_FAKE_CLIPBOARD_MIMES", "text/plain")
        .env("GIGATYPE_FAKE_KEY_LOG", &key_log)
        .env("GIGATYPE_PASTE_KEYS", "ctrl+shift+v")
        .env("GIGATYPE_CLIPBOARD_RESTORE_MS", "7")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(key_log).unwrap(), "ctrl+shift+v\n");
    assert!(std::fs::read_to_string(clipboard_log)
        .unwrap()
        .contains("wait:7ms\n"));
}
