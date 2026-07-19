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

fn daemon_command(runtime: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gigatype"));
    command
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("HOME", runtime)
        .env("GIGATYPE_PYTHON", "python3")
        .env("GIGATYPE_FAKE_TRANSCRIPT", "это тест")
        .env("GIGATYPE_FAKE_RECORDING", "1")
        .env("GIGATYPE_NO_INSERT", "1")
        .env("GIGATYPE_NO_NOTIFY", "1")
        .env("GIGATYPE_NO_SESSION_MONITOR", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_for_socket(runtime: &std::path::Path) {
    let socket = runtime.join("gigatype.sock");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "daemon should create its control socket");
}

#[test]
fn toggle_starts_and_stops_one_recording() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let sound_log = runtime.join("sounds.log");
    std::fs::write(&sound_log, "").unwrap();
    let daemon = daemon_command(runtime)
        .env("GIGATYPE_SOUND_LOG", &sound_log)
        .env("GIGATYPE_DEBOUNCE_MS", "0")
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

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

#[test]
fn successful_dictation_logs_the_complete_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let diagnostic_log = runtime.join("daemon.log");
    let daemon = daemon_command(runtime)
        .env("GIGATYPE_DEBOUNCE_MS", "0")
        .stderr(std::fs::File::create(&diagnostic_log).unwrap())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    assert!(run(runtime, "toggle").status.success());

    let diagnostics = std::fs::read_to_string(diagnostic_log).unwrap();
    assert!(diagnostics.contains("GigaType state: idle -> recording"));
    assert!(diagnostics.contains("GigaType state: recording -> transcribing"));
    assert!(diagnostics.contains("GigaType transcription: completed in"));
    assert!(diagnostics.contains("GigaType state: transcribing -> idle"));
}

#[test]
fn silent_recording_still_confirms_stop_and_logs_the_rejection() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let sound_log = runtime.join("sounds.log");
    let diagnostic_log = runtime.join("daemon.log");
    std::fs::write(&sound_log, "").unwrap();
    let daemon = daemon_command(runtime)
        .env("GIGATYPE_SOUND_LOG", &sound_log)
        .env("GIGATYPE_FAKE_NO_SPEECH", "1")
        .env("GIGATYPE_DEBOUNCE_MS", "0")
        .stderr(std::fs::File::create(&diagnostic_log).unwrap())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    let stop = run(runtime, "toggle");
    assert!(!stop.status.success());
    assert!(String::from_utf8_lossy(&stop.stderr).contains("no speech was detected"));
    assert_eq!(
        std::fs::read_to_string(sound_log).unwrap(),
        "listening\ntranscribing\n"
    );
    let diagnostics = std::fs::read_to_string(diagnostic_log).unwrap();
    assert!(diagnostics.contains("GigaType state: recording -> transcribing"));
    assert!(diagnostics.contains("GigaType speech: rejected as silence"));
}

#[test]
fn toggle_during_transcription_is_rejected_instead_of_queued() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let clipboard_log = runtime.join("clipboard.log");
    let key_log = runtime.join("keys.log");
    let mut command = daemon_command(runtime);
    command
        .env_remove("GIGATYPE_NO_INSERT")
        .env("GIGATYPE_FAKE_CLIPBOARD_LOG", &clipboard_log)
        .env("GIGATYPE_FAKE_CLIPBOARD_MIMES", "text/plain")
        .env("GIGATYPE_FAKE_KEY_LOG", &key_log)
        .env("GIGATYPE_CLIPBOARD_RESTORE_MS", "800")
        .env("GIGATYPE_DEBOUNCE_MS", "0");
    let daemon = command.spawn().unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    let runtime_for_stop = runtime.to_path_buf();
    let stop = thread::spawn(move || run(&runtime_for_stop, "toggle"));
    let marker = runtime.join("gigatype-transcribing");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "daemon should expose the transcribing state"
    );

    let started = Instant::now();
    let duplicate = run(runtime, "toggle");
    assert!(duplicate.status.success());
    assert_eq!(String::from_utf8_lossy(&duplicate.stdout), "transcribing\n");
    assert!(started.elapsed() < Duration::from_millis(250));

    assert!(stop.join().unwrap().status.success());
    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "status").stdout),
        "idle\n"
    );
}

#[test]
fn ignores_accidental_double_press_then_allows_cancel() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let daemon = daemon_command(runtime)
        .env("GIGATYPE_DEBOUNCE_MS", "300")
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    let duplicate = run(runtime, "toggle");
    assert!(duplicate.status.success());
    assert_eq!(String::from_utf8_lossy(&duplicate.stdout), "recording\n");
    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "cancel").stdout),
        "cancelled\n"
    );
}

#[test]
fn maximum_duration_stops_and_transcribes_instead_of_discarding() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let daemon = daemon_command(runtime)
        .env("GIGATYPE_MAX_RECORDING_MS", "80")
        .env("GIGATYPE_DEBOUNCE_MS", "0")
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if String::from_utf8_lossy(&run(runtime, "status").stdout) == "idle\n" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "status").stdout),
        "idle\n"
    );
    let history = runtime.join(".local/share/gigatype/history.jsonl");
    assert!(
        std::fs::read_to_string(history)
            .unwrap()
            .contains("Это тест."),
        "the timed-out recording should still be transcribed"
    );
}

#[test]
fn startup_removes_recording_left_by_an_interrupted_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let stale = runtime.join("gigatype-recording.wav");
    std::fs::write(&stale, "stale private audio").unwrap();

    let daemon = daemon_command(runtime).spawn().unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(!stale.exists());
}

#[test]
fn pauses_playing_media_only_while_recording() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let media_log = runtime.join("media.log");
    let daemon = daemon_command(runtime)
        .env("GIGATYPE_MEDIA_LOG", &media_log)
        .env("GIGATYPE_DEBOUNCE_MS", "0")
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    assert!(run(runtime, "toggle").status.success());
    assert_eq!(
        std::fs::read_to_string(media_log).unwrap(),
        "pause\nresume\n"
    );
}

#[test]
fn locking_the_session_cancels_private_audio_without_transcribing() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let lock_marker = runtime.join("locked");
    let media_log = runtime.join("media.log");
    let daemon = daemon_command(runtime)
        .env("GIGATYPE_FAKE_SESSION_LOCKED_FILE", &lock_marker)
        .env("GIGATYPE_MEDIA_LOG", &media_log)
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    std::fs::write(&lock_marker, "locked").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if String::from_utf8_lossy(&run(runtime, "status").stdout) == "idle\n" {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "status").stdout),
        "idle\n"
    );
    assert!(!runtime.join("gigatype-recording.wav").exists());
    assert!(!runtime.join(".local/share/gigatype/history.jsonl").exists());
    assert_eq!(
        std::fs::read_to_string(media_log).unwrap(),
        "pause\nresume\n"
    );
}

#[test]
fn unavailable_preferred_microphone_falls_back_to_the_system_default() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let recorder = runtime.join("fake-recorder.sh");
    let recorder_log = runtime.join("recorder.log");
    std::fs::write(
        &recorder,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *--target*) exit 1 ;;\nesac\nfor out do :; done\nprintf audio > \"$out\"\nwhile :; do sleep 1; done\n",
            recorder_log.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&recorder).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&recorder, permissions).unwrap();

    let mut command = daemon_command(runtime);
    command
        .env_remove("GIGATYPE_FAKE_RECORDING")
        .env("GIGATYPE_RECORDER", &recorder)
        .env("GIGATYPE_MICROPHONE", "missing-mic");
    let daemon = command.spawn().unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    let start = run(runtime, "toggle");
    assert!(start.status.success(), "{:?}", start);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline
        && std::fs::read_to_string(&recorder_log)
            .map(|calls| calls.lines().count())
            .unwrap_or(0)
            < 2
    {
        thread::sleep(Duration::from_millis(10));
    }
    let calls = std::fs::read_to_string(&recorder_log).unwrap();
    assert!(calls
        .lines()
        .next()
        .unwrap()
        .contains("--target missing-mic"));
    assert_eq!(
        calls.lines().count(),
        2,
        "default recorder should be retried"
    );
    assert!(!calls.lines().nth(1).unwrap().contains("--target"));
    assert!(run(runtime, "cancel").status.success());
}

#[test]
fn failed_auto_insert_leaves_the_transcript_on_the_clipboard() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let clipboard_log = runtime.join("clipboard.log");
    let fallback = runtime.join("fallback.txt");
    let mut command = daemon_command(runtime);
    command
        .env_remove("GIGATYPE_NO_INSERT")
        .env("GIGATYPE_FAKE_CLIPBOARD_LOG", &clipboard_log)
        .env("GIGATYPE_FAKE_CLIPBOARD_MIMES", "image/png")
        .env("GIGATYPE_FAKE_INSERT_ERROR", "1")
        .env("GIGATYPE_FAKE_FALLBACK_CLIPBOARD", &fallback)
        .env("GIGATYPE_CLIPBOARD_RESTORE_MS", "0")
        .env("GIGATYPE_DEBOUNCE_MS", "0");
    let daemon = command.spawn().unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    assert!(run(runtime, "toggle").status.success());
    assert_eq!(std::fs::read_to_string(fallback).unwrap(), "Это тест.");
}

#[test]
#[cfg(target_os = "linux")]
fn stalled_clipboard_provider_does_not_block_future_dictation() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path();
    let diagnostic_log = runtime.join("daemon.log");
    let sound_log = runtime.join("sounds.log");
    let insert_log = runtime.join("insert.log");
    let clipboard_log = runtime.join("clipboard.log");
    let daemon = daemon_command(runtime)
        .env_remove("GIGATYPE_NO_INSERT")
        .env("GIGATYPE_DEBOUNCE_MS", "0")
        .env("GIGATYPE_SOUND_LOG", &sound_log)
        .env("GIGATYPE_FAKE_INSERT_LOG", &insert_log)
        .env("GIGATYPE_FAKE_CLIPBOARD_LOG", &clipboard_log)
        .env("GIGATYPE_FAKE_CLIPBOARD_STALL", "1")
        .env("GIGATYPE_CLIPBOARD_TIMEOUT_MS", "50")
        .env("GIGATYPE_CLIPBOARD_RESTORE_MS", "0")
        .stderr(std::fs::File::create(&diagnostic_log).unwrap())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(daemon);
    wait_for_socket(runtime);

    assert!(run(runtime, "toggle").status.success());
    let started = Instant::now();
    let stop = run(runtime, "toggle");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "clipboard timeout must return control to F9"
    );
    assert!(stop.status.success());
    assert_eq!(String::from_utf8_lossy(&stop.stdout), "Это тест.\n");
    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "status").stdout),
        "idle\n"
    );

    assert!(run(runtime, "toggle").status.success());
    assert_eq!(
        String::from_utf8_lossy(&run(runtime, "status").stdout),
        "recording\n"
    );
    assert!(run(runtime, "cancel").status.success());
    assert_eq!(
        std::fs::read_to_string(sound_log).unwrap(),
        "listening\ntranscribing\nlistening\n"
    );
    let diagnostics = std::fs::read_to_string(diagnostic_log).unwrap();
    assert!(
        diagnostics.contains("preservation skipped"),
        "diagnostics were: {diagnostics}"
    );
}
