use std::process::Command;

#[test]
fn postprocesses_basic_russian_dictation() {
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["postprocess", "привет мир"])
        .output()
        .expect("gigatype should start");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Привет мир.\n");
}

#[test]
fn turns_spoken_russian_punctuation_into_text() {
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args([
            "postprocess",
            "привет запятая мир точка новая строка как дела вопросительный знак",
        ])
        .output()
        .expect("gigatype should start");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Привет, мир.\nКак дела?\n"
    );
}

#[test]
fn leaves_silence_empty_instead_of_inventing_punctuation() {
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["postprocess", "   "])
        .output()
        .expect("gigatype should start");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "\n");
}

#[test]
fn does_not_find_voice_commands_inside_normal_words() {
    let output = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .args(["postprocess", "уточка плывёт"])
        .output()
        .expect("gigatype should start");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Уточка плывёт.\n");
}
