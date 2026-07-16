use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(runtime: &std::path::Path, command: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .arg(command)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .unwrap()
}

#[test]
fn toggle_starts_and_stops_one_recording() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let sound_log = runtime.join("sounds.log");
    std::fs::write(&sound_log, "").unwrap();
    let daemon = Command::new(env!("CARGO_BIN_EXE_gigatype"))
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("HOME", runtime)
        .env("GIGATYPE_PYTHON", "python3")
        .env("GIGATYPE_FAKE_TRANSCRIPT", "это тест")
        .env("GIGATYPE_FAKE_RECORDING", "1")
        .env("GIGATYPE_NO_INSERT", "1")
        .env("GIGATYPE_SOUND_LOG", &sound_log)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);

    let socket = runtime.join("gigatype.sock");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "daemon should create its control socket");

    let start = run(runtime, "toggle");
    assert!(start.status.success());
    assert_eq!(String::from_utf8_lossy(&start.stdout), "recording\n");
    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "status").stdout),
        "recording\n"
    );

    let stop = run(runtime, "toggle");
    assert!(stop.status.success());
    assert_eq!(String::from_utf8_lossy(&stop.stdout), "Это тест.\n");
    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "status").stdout),
        "idle\n"
    );
    assert_eq!(
        std::fs::read_to_string(sound_log).unwrap(),
        "listening\ntranscribing\n"
    );
    assert!(
        !runtime.join("gigatype-recording.wav").exists(),
        "completed dictation audio should be deleted"
    );
    let history = runtime.join(".local/share/gigatype/history.jsonl");
    assert_eq!(
        std::fs::metadata(history).unwrap().permissions().mode() & 0o777,
        0o600,
        "transcript history should only be readable by its owner"
    );
}
