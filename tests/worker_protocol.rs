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

#[test]
fn worker_defaults_to_the_local_v3_end_to_end_rnnt_loader() {
    let worker = format!("{}/worker/gigaam_worker.py", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("python3")
        .args([worker.as_str(), "--print-model-plan"])
        .env("HOME", "/home/tester")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "model=/home/tester/.local/share/russian-asr/gigaam-v3-e2e-rnnt\nloader=local-e2e-rnnt\n"
    );
}
