use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn worker_speaks_one_json_object_per_line() {
    let worker = format!("{}/worker/gigaam_worker.py", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new("python3")
        .arg(worker)
        .env("GIGATYPE_FAKE_TRANSCRIPT", "привет из модели")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("worker should start");

    writeln!(
        child.stdin.as_mut().unwrap(),
        "{{\"id\":7,\"audio_path\":\"/tmp/test.wav\"}}"
    )
    .unwrap();
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["text"], "привет из модели");
}
