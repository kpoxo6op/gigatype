mod audio;
mod model;
mod platform;
#[cfg(target_os = "linux")]
mod portal;
mod shortcuts;

use std::env;
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Read;
use std::io::{BufRead, BufReader, Write};
#[cfg(target_os = "linux")]
use std::mem::size_of;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
#[cfg(target_os = "linux")]
use wl_clipboard_rs::{copy, paste};

const HELP: &str = "GigaType — local Russian voice typing with GigaAM

Usage: gigatype <command>

Commands:
  toggle       Start speaking, or stop and insert the transcription
  cancel       Discard the current recording
  status       Show whether GigaType is idle, recording, or transcribing
  daemon       Run the background service
  doctor       Check audio, model, desktop, and text-insertion support
  microphones  List microphones and mark the system default
  platform     Show the selected Unix platform adapters
  authorize    Grant Wayland portal permissions for shortcuts and insertion
  transcribe-file  Transcribe an audio file without typing it
  postprocess  Format raw transcript text
  help         Show this help
";

fn postprocess(input: &str) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut normalized = format!(" {normalized} ");
    for (spoken, symbol) in [
        ("вопросительный знак", "?"),
        ("восклицательный знак", "!"),
        ("новая строка", "\n"),
        ("двоеточие", ":"),
        ("точка с запятой", ";"),
        ("многоточие", "…"),
        ("запятая", ","),
        ("точка", "."),
    ] {
        normalized = normalized.replace(&format!(" {spoken} "), &format!(" {symbol} "));
    }
    normalized = normalized.trim().to_string();
    for punctuation in [",", ".", "!", "?", ":", ";", "…"] {
        normalized = normalized.replace(&format!(" {punctuation}"), punctuation);
    }

    let mut result = normalized
        .lines()
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join("\n");

    if result.is_empty() {
        return result;
    }

    if !result.ends_with(['.', '!', '?', '…']) {
        result.push('.');
    }
    result
}

fn capitalize_first(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

enum Transcriber {
    Fake(String),
    Native(Box<model::GigaAm>),
}

impl Transcriber {
    fn start() -> Result<Self, String> {
        match env::var("GIGATYPE_FAKE_TRANSCRIPT") {
            Ok(text) => Ok(Self::Fake(text)),
            Err(_) => model::GigaAm::load_default()
                .map(Box::new)
                .map(Self::Native),
        }
    }

    fn transcribe(&mut self, audio_path: &Path) -> Result<String, String> {
        match self {
            Self::Fake(text) => Ok(text.clone()),
            Self::Native(model) => model.transcribe_file(audio_path),
        }
    }
}

fn dirs_home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/gigatype-{}", unsafe { libc::geteuid() })))
}

fn socket_path() -> PathBuf {
    runtime_dir().join("gigatype.sock")
}

fn transcribing_path() -> PathBuf {
    runtime_dir().join("gigatype-transcribing")
}

struct TranscribingGuard {
    path: PathBuf,
}

impl TranscribingGuard {
    fn start() -> Result<Self, String> {
        let path = transcribing_path();
        fs::write(&path, "transcribing\n").map_err(|error| error.to_string())?;
        eprintln!("GigaType state: recording -> transcribing");
        Ok(Self { path })
    }
}

impl Drop for TranscribingGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        eprintln!("GigaType state: transcribing -> idle");
    }
}

enum DictationState {
    Idle,
    Recording {
        recorder: RecorderHandle,
        audio_path: PathBuf,
        started_at: Instant,
        media: MediaGuard,
        can_fallback_microphone: bool,
    },
}

enum RecorderHandle {
    Native(audio::Recorder),
    Process(Child),
    Fake,
}

struct MediaGuard {
    paused_services: Vec<String>,
}

impl MediaGuard {
    fn pause_playing() -> Self {
        if let Some(path) = env::var_os("GIGATYPE_MEDIA_LOG") {
            let _ = fs::write(path, "pause\n");
            return Self {
                paused_services: vec!["fake".into()],
            };
        }
        if env::var_os("GIGATYPE_KEEP_MEDIA_PLAYING").is_some() {
            return Self {
                paused_services: Vec::new(),
            };
        }
        let output = Command::new("qdbus6").output();
        let mut paused_services = Vec::new();
        if let Ok(output) = output {
            for service in String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with("org.mpris.MediaPlayer2."))
            {
                let status = Command::new("gdbus")
                    .args([
                        "call",
                        "--session",
                        "--dest",
                        service,
                        "--object-path",
                        "/org/mpris/MediaPlayer2",
                        "--method",
                        "org.freedesktop.DBus.Properties.Get",
                        "org.mpris.MediaPlayer2.Player",
                        "PlaybackStatus",
                    ])
                    .output();
                let is_playing = status.is_ok_and(|status| {
                    status.status.success()
                        && String::from_utf8_lossy(&status.stdout).contains("Playing")
                });
                if is_playing
                    && Command::new("gdbus")
                        .args([
                            "call",
                            "--session",
                            "--dest",
                            service,
                            "--object-path",
                            "/org/mpris/MediaPlayer2",
                            "--method",
                            "org.mpris.MediaPlayer2.Player.Pause",
                        ])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|status| status.success())
                {
                    paused_services.push(service.to_string());
                }
            }
        }
        Self { paused_services }
    }

    fn resume(self) {
        if let Some(path) = env::var_os("GIGATYPE_MEDIA_LOG") {
            if !self.paused_services.is_empty() {
                if let Ok(mut file) = OpenOptions::new().append(true).open(path) {
                    let _ = writeln!(file, "resume");
                }
            }
            return;
        }
        for service in self.paused_services {
            let _ = Command::new("gdbus")
                .args([
                    "call",
                    "--session",
                    "--dest",
                    &service,
                    "--object-path",
                    "/org/mpris/MediaPlayer2",
                    "--method",
                    "org.mpris.MediaPlayer2.Player.Play",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[derive(Copy, Clone)]
enum SoundCue {
    Listening,
    Transcribing,
}

fn play_sound(cue: SoundCue) {
    if env::var_os("GIGATYPE_NO_SOUNDS").is_some() {
        return;
    }
    let (label, variable, fallback) = match cue {
        SoundCue::Listening => (
            "listening",
            "GIGATYPE_START_SOUND",
            "/usr/share/sounds/freedesktop/stereo/device-added.oga",
        ),
        SoundCue::Transcribing => (
            "transcribing",
            "GIGATYPE_STOP_SOUND",
            "/usr/share/sounds/freedesktop/stereo/device-removed.oga",
        ),
    };
    let result = if let Some(log) = env::var_os("GIGATYPE_SOUND_LOG") {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
            writeln!(file, "{label}").map_err(|error| error.to_string())
        } else {
            Err("could not open the sound log".into())
        }
    } else {
        let custom = env::var_os(variable).map(PathBuf::from);
        let sound = custom.clone().unwrap_or_else(|| PathBuf::from(fallback));
        if custom.is_some() && sound.is_file() {
            let player = if cfg!(target_os = "macos") {
                "afplay"
            } else {
                "pw-play"
            };
            Command::new(player)
                .arg(sound)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|error| error.to_string())
                .and_then(|status| {
                    status
                        .success()
                        .then_some(())
                        .ok_or_else(|| format!("{player} exited with {status}"))
                })
        } else {
            audio::play_cue(matches!(cue, SoundCue::Listening))
        }
    };
    if let Err(error) = result {
        eprintln!("GigaType cue: {label} failed: {error}");
    } else {
        eprintln!("GigaType cue: {label} played");
    }
}

fn notify(summary: &str, body: &str) {
    if env::var_os("GIGATYPE_NO_NOTIFY").is_some() {
        return;
    }
    if cfg!(target_os = "macos") {
        let script = format!("display notification {:?} with title {:?}", body, summary);
        let _ = Command::new("osascript")
            .args(["-e", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        return;
    }
    let _ = Command::new("notify-send")
        .args([
            "--app-name=GigaType",
            "--icon=audio-input-microphone",
            summary,
            body,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn spawn_recorder(audio_path: &Path, target: Option<&str>) -> Result<Child, String> {
    let recorder = env::var_os("GIGATYPE_RECORDER").unwrap_or_else(|| "pw-record".into());
    let mut command = Command::new(recorder);
    command.args(["--rate", "16000", "--channels", "1", "--format", "s16"]);
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    command
        .arg(audio_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start microphone recording: {error}"))
}

fn start_recording() -> Result<DictationState, String> {
    let audio_path = runtime_dir().join("gigatype-recording.wav");
    let _ = fs::remove_file(&audio_path);
    let media = MediaGuard::pause_playing();
    play_sound(SoundCue::Listening);
    let (recorder, can_fallback_microphone) = if env::var_os("GIGATYPE_FAKE_RECORDING").is_some() {
        fs::write(&audio_path, b"fake audio").map_err(|error| error.to_string())?;
        (RecorderHandle::Fake, false)
    } else if env::var_os("GIGATYPE_RECORDER").is_none() {
        let preferred = env::var("GIGATYPE_MICROPHONE").ok();
        match audio::Recorder::start(preferred.as_deref()) {
            Ok(recorder) => (RecorderHandle::Native(recorder), preferred.is_some()),
            Err(_) if preferred.is_some() => {
                notify(
                    "Microphone fallback",
                    "Preferred microphone is unavailable; using the system default",
                );
                match audio::Recorder::start(None) {
                    Ok(recorder) => (RecorderHandle::Native(recorder), false),
                    Err(error) => {
                        media.resume();
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                media.resume();
                return Err(error);
            }
        }
    } else {
        let preferred = env::var("GIGATYPE_MICROPHONE").ok();
        let mut process = match spawn_recorder(&audio_path, preferred.as_deref()) {
            Ok(process) => process,
            Err(error) => {
                media.resume();
                return Err(error);
            }
        };
        if preferred.is_some() {
            thread::sleep(Duration::from_millis(80));
            if process
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
            {
                notify(
                    "Microphone fallback",
                    "Preferred microphone is unavailable; using the system default",
                );
                process = match spawn_recorder(&audio_path, None) {
                    Ok(process) => process,
                    Err(error) => {
                        media.resume();
                        return Err(error);
                    }
                };
                (RecorderHandle::Process(process), false)
            } else {
                (RecorderHandle::Process(process), true)
            }
        } else {
            (RecorderHandle::Process(process), false)
        }
    };
    notify("Listening…", "Press the shortcut again to transcribe");
    Ok(DictationState::Recording {
        recorder,
        audio_path,
        started_at: Instant::now(),
        media,
        can_fallback_microphone,
    })
}

fn recover_disconnected_microphone(state: &mut DictationState) {
    let DictationState::Recording {
        recorder: RecorderHandle::Process(process),
        audio_path,
        can_fallback_microphone,
        ..
    } = state
    else {
        return;
    };
    if !*can_fallback_microphone {
        return;
    }
    let disconnected = process.try_wait().is_ok_and(|status| status.is_some());
    if disconnected {
        *can_fallback_microphone = false;
        match spawn_recorder(audio_path, None) {
            Ok(replacement) => {
                *process = replacement;
                notify(
                    "Microphone changed",
                    "Preferred microphone disconnected; recording continues on the system default",
                );
            }
            Err(error) => notify("GigaType error", &error),
        }
    }
}

fn stop_recording(recorder: RecorderHandle, audio_path: &Path) -> Result<(), String> {
    match recorder {
        RecorderHandle::Native(recorder) => return recorder.stop(audio_path),
        RecorderHandle::Fake => {}
        RecorderHandle::Process(mut process) => {
            unsafe { libc::kill(process.id() as i32, libc::SIGINT) };
            for _ in 0..50 {
                if process
                    .try_wait()
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if process
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_none()
            {
                process.kill().map_err(|error| error.to_string())?;
                process.wait().map_err(|error| error.to_string())?;
            }
        }
    }
    let size = fs::metadata(audio_path).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err("the microphone produced no audio".into());
    }
    Ok(())
}

fn audio_has_speech(audio_path: &Path) -> Result<bool, String> {
    let bytes = fs::read(audio_path).map_err(|error| error.to_string())?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Ok(true);
    }
    let mut cursor = 12_usize;
    let mut pcm_16_bit = false;
    let mut samples = None;
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let start = cursor + 8;
        let end = start.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end >= start + 16 {
            let format = u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap());
            let bits = u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap());
            pcm_16_bit = format == 1 && bits == 16;
        } else if id == b"data" {
            samples = Some(&bytes[start..end]);
        }
        cursor = start.saturating_add(size + (size % 2));
    }
    let Some(data) = samples else {
        return Ok(true);
    };
    if !pcm_16_bit || data.len() < 2 {
        return Ok(true);
    }
    let threshold = env::var("GIGATYPE_SILENCE_RMS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(80.0);
    let mut squared = 0_f64;
    let mut loud_samples = 0_usize;
    let mut sample_count = 0_usize;
    let mut frame_squared = 0_f64;
    let mut frame_loud_samples = 0_usize;
    let mut frame_sample_count = 0_usize;
    let mut consecutive_voiced_frames = 0_usize;
    let mut longest_voice_run = 0_usize;
    const FRAME_SAMPLES: usize = 1_600;
    for sample in data.chunks_exact(2) {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as f64;
        squared += value * value;
        frame_squared += value * value;
        if value.abs() >= threshold * 4.0 {
            loud_samples += 1;
            frame_loud_samples += 1;
        }
        sample_count += 1;
        frame_sample_count += 1;
        if frame_sample_count == FRAME_SAMPLES {
            let frame_rms = (frame_squared / frame_sample_count as f64).sqrt();
            if frame_rms >= threshold && frame_loud_samples * 20 >= frame_sample_count {
                consecutive_voiced_frames += 1;
                longest_voice_run = longest_voice_run.max(consecutive_voiced_frames);
            } else {
                consecutive_voiced_frames = 0;
            }
            frame_squared = 0.0;
            frame_loud_samples = 0;
            frame_sample_count = 0;
        }
    }
    let rms = (squared / sample_count as f64).sqrt();
    let has_speech = longest_voice_run >= 2;
    eprintln!(
        "GigaType audio: rms={rms:.2}, threshold={threshold:.2}, loud_samples={loud_samples}/{sample_count}, longest_voice_run={longest_voice_run}, speech={has_speech}"
    );
    Ok(has_speech)
}

fn save_history(text: &str) -> Result<(), String> {
    let dir = dirs_home().join(".local/share/gigatype");
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let history_path = dir.join("history.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&history_path)
        .map_err(|error| error.to_string())?;
    fs::set_permissions(&history_path, fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({"timestamp": timestamp, "text": text}),
    )
    .map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
static DESKTOP_CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();

#[cfg(target_os = "linux")]
fn with_desktop_clipboard<T>(
    operation: impl FnOnce(&mut arboard::Clipboard) -> Result<T, String>,
) -> Result<T, String> {
    let mut owner = DESKTOP_CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "desktop clipboard lock was poisoned".to_string())?;
    if owner.is_none() {
        *owner = Some(
            arboard::Clipboard::new()
                .map_err(|error| format!("could not open the desktop clipboard: {error}"))?,
        );
    }
    operation(owner.as_mut().expect("desktop clipboard was initialized"))
}

fn put_on_clipboard(text: &str) -> Result<(), String> {
    if let Some(path) = env::var_os("GIGATYPE_FAKE_FALLBACK_CLIPBOARD") {
        return fs::write(path, text).map_err(|error| error.to_string());
    }
    let status = Command::new("qdbus6")
        .args([
            "org.kde.klipper",
            "/klipper",
            "org.kde.klipper.klipper.setClipboardContents",
            text,
        ])
        .status();
    if status.is_ok_and(|status| status.success()) {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    return with_desktop_clipboard(|clipboard| {
        clipboard
            .set_text(text)
            .map_err(|error| format!("could not set the desktop clipboard: {error}"))
    });
    #[cfg(not(target_os = "linux"))]
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| format!("could not open the desktop clipboard: {error}"))?;
    #[cfg(not(target_os = "linux"))]
    clipboard
        .set_text(text)
        .map_err(|error| format!("could not set the desktop clipboard: {error}"))
}

struct ClipboardEntry {
    mime_type: String,
    data: Vec<u8>,
}

struct ClipboardSnapshot {
    entries: Vec<ClipboardEntry>,
}

fn append_fake_clipboard_log(line: &str) -> Result<(), String> {
    let Some(path) = env::var_os("GIGATYPE_FAKE_CLIPBOARD_LOG") else {
        return Ok(());
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn clipboard_timeout() -> Duration {
    Duration::from_millis(
        env::var("GIGATYPE_CLIPBOARD_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1000)
            .clamp(10, 5000),
    )
}

#[cfg(target_os = "linux")]
fn read_clipboard_pipe_until<R: Read + AsRawFd>(
    reader: &mut R,
    deadline: Instant,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;
    let mut data = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "clipboard data transfer timed out after {}ms",
                timeout.as_millis()
            ));
        }
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("could not wait for clipboard data: {error}"));
        }
        if ready == 0 {
            return Err(format!(
                "clipboard data transfer timed out after {}ms",
                timeout.as_millis()
            ));
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err("clipboard data pipe became invalid".into());
        }
        if descriptor.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
            return Err("clipboard data pipe failed".into());
        }

        let mut chunk = [0_u8; 8192];
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(data),
            Ok(read) => {
                data.extend_from_slice(&chunk[..read]);
                if data.len() > MAX_CLIPBOARD_BYTES {
                    return Err("clipboard data exceeded the 64 MiB safety limit".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("could not read clipboard data: {error}")),
        }
    }
}

fn capture_clipboard() -> Result<ClipboardSnapshot, String> {
    #[cfg(target_os = "linux")]
    if env::var_os("GIGATYPE_FAKE_CLIPBOARD_STALL").is_some() {
        let (mut reader, _writer) = UnixStream::pair().map_err(|error| error.to_string())?;
        let timeout = clipboard_timeout();
        let deadline = Instant::now() + timeout;
        read_clipboard_pipe_until(&mut reader, deadline, timeout)?;
        return Err("fake stalled clipboard unexpectedly returned data".into());
    }
    if env::var_os("GIGATYPE_FAKE_CLIPBOARD_LOG").is_some() {
        let mime_types = env::var("GIGATYPE_FAKE_CLIPBOARD_MIMES").unwrap_or_default();
        append_fake_clipboard_log(&format!("capture:{mime_types}"))?;
        return Ok(ClipboardSnapshot {
            entries: mime_types
                .split(',')
                .filter(|mime| !mime.is_empty())
                .map(|mime_type| ClipboardEntry {
                    mime_type: mime_type.to_string(),
                    data: Vec::new(),
                })
                .collect(),
        });
    }

    #[cfg(target_os = "linux")]
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        let timeout = clipboard_timeout();
        let deadline = Instant::now() + timeout;
        let mime_types = match paste::get_mime_types_ordered(
            paste::ClipboardType::Regular,
            paste::Seat::Unspecified,
        ) {
            Ok(mime_types) => mime_types,
            Err(paste::Error::ClipboardEmpty | paste::Error::NoMimeType) => Vec::new(),
            Err(error) => return Err(format!("could not inspect the Wayland clipboard: {error}")),
        };
        let mut entries = Vec::with_capacity(mime_types.len());
        for mime_type in mime_types {
            let (mut pipe, _) = paste::get_contents(
                paste::ClipboardType::Regular,
                paste::Seat::Unspecified,
                paste::MimeType::Specific(&mime_type),
            )
            .map_err(|error| format!("could not preserve clipboard type {mime_type}: {error}"))?;
            let data =
                read_clipboard_pipe_until(&mut pipe, deadline, timeout).map_err(|error| {
                    format!("could not preserve clipboard type {mime_type}: {error}")
                })?;
            entries.push(ClipboardEntry { mime_type, data });
        }
        return Ok(ClipboardSnapshot { entries });
    }
    #[cfg(target_os = "linux")]
    let text = with_desktop_clipboard(|clipboard| Ok(clipboard.get_text().ok()))?;
    #[cfg(not(target_os = "linux"))]
    let text = arboard::Clipboard::new()
        .map_err(|error| format!("could not open the desktop clipboard: {error}"))?
        .get_text()
        .ok();
    let entries = text
        .map(|text| ClipboardEntry {
            mime_type: "text/plain;charset=utf-8".into(),
            data: text.into_bytes(),
        })
        .into_iter()
        .collect();
    Ok(ClipboardSnapshot { entries })
}

fn restore_clipboard(snapshot: ClipboardSnapshot) -> Result<(), String> {
    if env::var_os("GIGATYPE_FAKE_CLIPBOARD_LOG").is_some() {
        let mime_types = snapshot
            .entries
            .iter()
            .map(|entry| entry.mime_type.as_str())
            .collect::<Vec<_>>()
            .join(",");
        return append_fake_clipboard_log(&format!("restore:{mime_types}"));
    }
    #[cfg(target_os = "linux")]
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        if snapshot.entries.is_empty() {
            return copy::clear(copy::ClipboardType::Regular, copy::Seat::All)
                .map_err(|error| format!("could not restore an empty Wayland clipboard: {error}"));
        }
        let sources = snapshot
            .entries
            .into_iter()
            .map(|entry| copy::MimeSource {
                source: copy::Source::Bytes(entry.data.into_boxed_slice()),
                mime_type: copy::MimeType::Specific(entry.mime_type),
            })
            .collect();
        let mut options = copy::Options::new();
        options.omit_additional_text_mime_types(true);
        return copy::copy_multi(options, sources)
            .map_err(|error| format!("could not restore the Wayland clipboard: {error}"));
    }
    let Some(entry) = snapshot
        .entries
        .into_iter()
        .find(|entry| entry.mime_type.starts_with("text/plain"))
    else {
        return Ok(());
    };
    let text = String::from_utf8(entry.data).map_err(|error| error.to_string())?;
    #[cfg(target_os = "linux")]
    return with_desktop_clipboard(|clipboard| {
        clipboard.set_text(text).map_err(|error| error.to_string())
    });
    #[cfg(not(target_os = "linux"))]
    arboard::Clipboard::new()
        .map_err(|error| error.to_string())?
        .set_text(text)
        .map_err(|error| error.to_string())
}

fn set_transcript_clipboard(text: &str) -> Result<(), String> {
    if env::var_os("GIGATYPE_FAKE_CLIPBOARD_LOG").is_some() {
        append_fake_clipboard_log("set:text/plain;charset=utf-8")?;
        return Ok(());
    }
    put_on_clipboard(text)
}

#[repr(C)]
#[cfg(target_os = "linux")]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
#[cfg(target_os = "linux")]
struct UinputUserDev {
    name: [u8; 80],
    id: InputId,
    ff_effects_max: u32,
    absmax: [i32; 64],
    absmin: [i32; 64],
    absfuzz: [i32; 64],
    absflat: [i32; 64],
}

#[repr(C)]
#[cfg(target_os = "linux")]
struct InputEvent {
    time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

#[cfg(target_os = "linux")]
fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

#[cfg(target_os = "linux")]
fn emit_key(file: &mut fs::File, code: u16, value: i32) -> Result<(), String> {
    let event = InputEvent {
        time: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        event_type: 1,
        code,
        value,
    };
    file.write_all(as_bytes(&event))
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn emit_sync(file: &mut fs::File) -> Result<(), String> {
    let event = InputEvent {
        time: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        event_type: 0,
        code: 0,
        value: 0,
    };
    file.write_all(as_bytes(&event))
        .map_err(|error| error.to_string())
}

#[derive(Copy, Clone)]
enum PasteChord {
    CtrlV,
    CtrlShiftV,
    ShiftInsert,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum LinuxKeyboardRoute {
    Portal,
    Enigo,
    Uinput,
}

#[cfg(target_os = "linux")]
fn linux_keyboard_route(
    wayland: bool,
    display: bool,
    desktop: &str,
    no_portal: bool,
) -> LinuxKeyboardRoute {
    if wayland {
        if !no_portal && !desktop.to_ascii_lowercase().contains("kde") {
            LinuxKeyboardRoute::Portal
        } else {
            LinuxKeyboardRoute::Uinput
        }
    } else if display {
        LinuxKeyboardRoute::Enigo
    } else {
        LinuxKeyboardRoute::Uinput
    }
}

impl PasteChord {
    fn from_env() -> Result<Self, String> {
        match env::var("GIGATYPE_PASTE_KEYS")
            .unwrap_or_else(|_| "ctrl+v".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "ctrl+v" => Ok(Self::CtrlV),
            "ctrl+shift+v" => Ok(Self::CtrlShiftV),
            "shift+insert" => Ok(Self::ShiftInsert),
            value => Err(format!(
                "unsupported paste keys {value:?}; use ctrl+v, ctrl+shift+v, or shift+insert"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::CtrlV => "ctrl+v",
            Self::CtrlShiftV => "ctrl+shift+v",
            Self::ShiftInsert => "shift+insert",
        }
    }

    #[cfg(target_os = "linux")]
    fn events(self) -> &'static [(u16, i32)] {
        const KEY_LEFTCTRL: u16 = 29;
        const KEY_LEFTSHIFT: u16 = 42;
        const KEY_V: u16 = 47;
        const KEY_INSERT: u16 = 110;
        match self {
            Self::CtrlV => &[(KEY_LEFTCTRL, 1), (KEY_V, 1), (KEY_V, 0), (KEY_LEFTCTRL, 0)],
            Self::CtrlShiftV => &[
                (KEY_LEFTCTRL, 1),
                (KEY_LEFTSHIFT, 1),
                (KEY_V, 1),
                (KEY_V, 0),
                (KEY_LEFTSHIFT, 0),
                (KEY_LEFTCTRL, 0),
            ],
            Self::ShiftInsert => &[
                (KEY_LEFTSHIFT, 1),
                (KEY_INSERT, 1),
                (KEY_INSERT, 0),
                (KEY_LEFTSHIFT, 0),
            ],
        }
    }
}

fn paste_shortcut(probe_only: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
    #[cfg(target_os = "linux")]
    const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
    #[cfg(target_os = "linux")]
    const UI_DEV_CREATE: libc::c_ulong = 0x5501;
    #[cfg(target_os = "linux")]
    const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
    #[cfg(target_os = "linux")]
    const KEY_LEFTCTRL: u16 = 29;
    #[cfg(target_os = "linux")]
    const KEY_LEFTSHIFT: u16 = 42;
    #[cfg(target_os = "linux")]
    const KEY_V: u16 = 47;
    #[cfg(target_os = "linux")]
    const KEY_INSERT: u16 = 110;

    let chord = PasteChord::from_env()?;
    if !probe_only && env::var_os("GIGATYPE_FAKE_INSERT_ERROR").is_some() {
        return Err("simulated insertion failure".into());
    }
    if let Some(path) = env::var_os("GIGATYPE_FAKE_KEY_LOG") {
        if !probe_only {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| error.to_string())?;
            writeln!(file, "{}", chord.name()).map_err(|error| error.to_string())?;
        }
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    let linux_route = linux_keyboard_route(
        env::var_os("WAYLAND_DISPLAY").is_some(),
        env::var_os("DISPLAY").is_some(),
        &env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        env::var_os("GIGATYPE_NO_PORTAL").is_some(),
    );

    #[cfg(target_os = "linux")]
    if linux_route == LinuxKeyboardRoute::Portal && portal::paste(chord, probe_only).is_ok() {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    let use_enigo = linux_route == LinuxKeyboardRoute::Enigo;
    #[cfg(not(target_os = "linux"))]
    let use_enigo = true;

    if use_enigo {
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|error| format!("could not open native keyboard adapter: {error}"))?;
        if probe_only {
            return Ok(());
        }
        let primary_modifier = if cfg!(target_os = "macos") {
            Key::Meta
        } else {
            Key::Control
        };
        let result = match chord {
            PasteChord::CtrlV => enigo.key(primary_modifier, Direction::Press).and_then(|_| {
                enigo
                    .key(Key::Unicode('v'), Direction::Click)
                    .and_then(|_| enigo.key(primary_modifier, Direction::Release))
            }),
            PasteChord::CtrlShiftV => {
                enigo.key(primary_modifier, Direction::Press).and_then(|_| {
                    enigo.key(Key::Shift, Direction::Press).and_then(|_| {
                        enigo
                            .key(Key::Unicode('v'), Direction::Click)
                            .and_then(|_| {
                                enigo
                                    .key(Key::Shift, Direction::Release)
                                    .and_then(|_| enigo.key(primary_modifier, Direction::Release))
                            })
                    })
                })
            }
            PasteChord::ShiftInsert => {
                #[cfg(target_os = "macos")]
                {
                    enigo.key(Key::Meta, Direction::Press).and_then(|_| {
                        enigo
                            .key(Key::Unicode('v'), Direction::Click)
                            .and_then(|_| enigo.key(Key::Meta, Direction::Release))
                    })
                }
                #[cfg(not(target_os = "macos"))]
                {
                    enigo.key(Key::Shift, Direction::Press).and_then(|_| {
                        enigo
                            .key(Key::Insert, Direction::Click)
                            .and_then(|_| enigo.key(Key::Shift, Direction::Release))
                    })
                }
            }
        };
        return result.map_err(|error| format!("native paste failed: {error}"));
    }

    #[cfg(not(target_os = "linux"))]
    return Err("native paste is unavailable".into());

    #[cfg(target_os = "linux")]
    {
        let mut device = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uinput")
            .map_err(|error| format!("cannot type into apps via /dev/uinput: {error}"))?;
        let fd = device.as_raw_fd();
        for (request, value) in [
            (UI_SET_EVBIT, 1),
            (UI_SET_KEYBIT, KEY_LEFTCTRL as libc::c_int),
            (UI_SET_KEYBIT, KEY_LEFTSHIFT as libc::c_int),
            (UI_SET_KEYBIT, KEY_V as libc::c_int),
            (UI_SET_KEYBIT, KEY_INSERT as libc::c_int),
        ] {
            if unsafe { libc::ioctl(fd, request, value) } < 0 {
                return Err(format!(
                    "could not configure virtual keyboard: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        let mut setup = UinputUserDev {
            name: [0; 80],
            id: InputId {
                bustype: 0x03,
                vendor: 0x4749,
                product: 0x5459,
                version: 1,
            },
            ff_effects_max: 0,
            absmax: [0; 64],
            absmin: [0; 64],
            absfuzz: [0; 64],
            absflat: [0; 64],
        };
        let name = b"GigaType virtual keyboard";
        setup.name[..name.len()].copy_from_slice(name);
        device
            .write_all(as_bytes(&setup))
            .map_err(|error| error.to_string())?;
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE) } < 0 {
            return Err(format!(
                "could not create virtual keyboard: {}",
                std::io::Error::last_os_error()
            ));
        }
        if probe_only {
            unsafe { libc::ioctl(fd, UI_DEV_DESTROY) };
            return Ok(());
        }
        thread::sleep(Duration::from_millis(120));
        for &(code, value) in chord.events() {
            emit_key(&mut device, code, value)?;
            emit_sync(&mut device)?;
        }
        thread::sleep(Duration::from_millis(80));
        unsafe { libc::ioctl(fd, UI_DEV_DESTROY) };
        Ok(())
    }
}

fn insert_text(text: &str) -> Result<(), String> {
    if env::var_os("GIGATYPE_FAKE_CLIPBOARD_LOG").is_none() {
        if let Some(path) = env::var_os("GIGATYPE_FAKE_INSERT_LOG") {
            return fs::write(path, text).map_err(|error| error.to_string());
        }
    }
    let previous = match capture_clipboard() {
        Ok(snapshot) => Some(snapshot),
        Err(error) => {
            eprintln!("GigaType clipboard: preservation skipped ({error})");
            None
        }
    };
    set_transcript_clipboard(text)?;
    let result = if let Some(path) = env::var_os("GIGATYPE_FAKE_INSERT_LOG") {
        fs::write(path, text).map_err(|error| error.to_string())
    } else {
        paste_shortcut(false)
    };
    let restore_ms = env::var("GIGATYPE_CLIPBOARD_RESTORE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(120)
        .min(5000);
    append_fake_clipboard_log(&format!("wait:{restore_ms}ms"))?;
    thread::sleep(Duration::from_millis(restore_ms));
    let restore = previous.map_or(Ok(()), restore_clipboard);
    result.and(restore)
}

fn command_exists(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

fn doctor() {
    let model = model::model_dir();
    let model_ok = model::model_is_complete(&model);
    println!(
        "{} Native GigaAM ONNX {}",
        if model_ok { "✓" } else { "✗" },
        model.display()
    );

    println!(
        "✓ Native microphone   CPAL ({})",
        cpal::default_host().id().name()
    );

    match audio::output_report() {
        Ok(report) => println!("✓ Audio output        {report}"),
        Err(error) => println!("✗ Audio output        {error}"),
    }

    let clipboard_ok = Command::new("qdbus6")
        .args([
            "org.kde.klipper",
            "/klipper",
            "org.kde.klipper.klipper.getClipboardContents",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    println!(
        "{} Clipboard adapter  {}",
        if clipboard_ok { "✓" } else { "✗" },
        platform::Platform::detect().insertion
    );

    let insert_ok = paste_shortcut(true).is_ok();
    println!(
        "{} Auto-insert         /dev/uinput{}",
        if insert_ok { "✓" } else { "✗" },
        if insert_ok {
            ""
        } else {
            " (installer must grant desktop access)"
        }
    );

    let media_ok = command_exists("gdbus") && command_exists("qdbus6");
    println!(
        "{} Media pause/resume MPRIS",
        if media_ok { "✓" } else { "✗" }
    );

    let session_ok = Command::new("qdbus6")
        .args([
            "org.freedesktop.ScreenSaver",
            "/ScreenSaver",
            "org.freedesktop.ScreenSaver.GetActive",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    println!(
        "{} Session safety     lock/suspend cleanup",
        if session_ok { "✓" } else { "✗" }
    );

    let service = send_daemon("status").unwrap_or_else(|_| "not running".into());
    println!(
        "{} Background service  {service}",
        if service == "not running" {
            "○"
        } else {
            "✓"
        }
    );
}

fn finish_dictation(
    transcriber: &mut Transcriber,
    recorder: RecorderHandle,
    audio_path: &Path,
    media: MediaGuard,
) -> Result<String, String> {
    let _transcribing = TranscribingGuard::start()?;
    let stopped = stop_recording(recorder, audio_path);
    media.resume();
    play_sound(SoundCue::Transcribing);
    stopped?;
    let no_speech = env::var_os("GIGATYPE_FAKE_NO_SPEECH").is_some()
        || (env::var_os("GIGATYPE_FAKE_RECORDING").is_none() && !audio_has_speech(audio_path)?);
    if no_speech {
        let _ = fs::remove_file(audio_path);
        eprintln!("GigaType speech: rejected as silence");
        notify(
            "No speech detected",
            "Recording was silent and was not transcribed",
        );
        return Err("no speech was detected".into());
    }
    notify("Transcribing…", "GigaAM is processing your speech locally");
    let transcription_started = Instant::now();
    let transcription = transcriber.transcribe(audio_path);
    match &transcription {
        Ok(_) => eprintln!(
            "GigaType transcription: completed in {:.2}s",
            transcription_started.elapsed().as_secs_f64()
        ),
        Err(error) => eprintln!(
            "GigaType transcription: failed after {:.2}s: {error}",
            transcription_started.elapsed().as_secs_f64()
        ),
    }
    let cleanup = fs::remove_file(audio_path)
        .map_err(|error| format!("could not delete temporary recording: {error}"));
    let text = postprocess(&transcription?);
    cleanup?;
    if text.is_empty() {
        return Err("no speech was recognized".into());
    }
    save_history(&text)?;
    if env::var_os("GIGATYPE_NO_INSERT").is_none() {
        if let Err(error) = insert_text(&text) {
            put_on_clipboard(&text)?;
            eprintln!("GigaType insertion: failed ({error}); copied to clipboard");
            notify(
                "Text copied",
                &format!("Auto-insert unavailable ({error}). Press Ctrl+V."),
            );
        } else {
            eprintln!("GigaType insertion: completed");
            notify("Inserted", "Russian transcription typed at the cursor");
        }
    }
    Ok(text)
}

fn session_is_locked() -> bool {
    if let Some(marker) = env::var_os("GIGATYPE_FAKE_SESSION_LOCKED_FILE") {
        return Path::new(&marker).exists();
    }
    if env::var_os("GIGATYPE_NO_SESSION_MONITOR").is_some() {
        return false;
    }
    Command::new("qdbus6")
        .args([
            "org.freedesktop.ScreenSaver",
            "/ScreenSaver",
            "org.freedesktop.ScreenSaver.GetActive",
        ])
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true"
        })
}

fn boottime() -> Duration {
    #[cfg(target_os = "linux")]
    {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } == 0 {
            Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
        } else {
            Duration::ZERO
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Duration::ZERO
    }
}

struct SuspendDetector {
    instant: Instant,
    boottime: Duration,
}

impl SuspendDetector {
    fn new() -> Self {
        Self {
            instant: Instant::now(),
            boottime: boottime(),
        }
    }

    fn resumed(&mut self) -> bool {
        let now_instant = Instant::now();
        let now_boottime = boottime();
        let awake = now_instant.duration_since(self.instant);
        let boot = now_boottime.saturating_sub(self.boottime);
        self.instant = now_instant;
        self.boottime = now_boottime;
        boot.saturating_sub(awake) > Duration::from_millis(500)
    }
}

fn discard_recording(state: &mut DictationState, notification: (&str, &str)) -> bool {
    let DictationState::Recording {
        recorder,
        audio_path,
        media,
        ..
    } = std::mem::replace(state, DictationState::Idle)
    else {
        return false;
    };
    if let RecorderHandle::Process(mut process) = recorder {
        let _ = process.kill();
        let _ = process.wait();
    }
    let _ = fs::remove_file(audio_path);
    media.resume();
    notify(notification.0, notification.1);
    true
}

fn daemon() -> Result<(), String> {
    fs::create_dir_all(runtime_dir()).map_err(|error| error.to_string())?;
    let _ = fs::remove_file(runtime_dir().join("gigatype-recording.wav"));
    let _ = fs::remove_file(transcribing_path());
    let path = socket_path();
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .map_err(|error| format!("could not bind {}: {error}", path.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure daemon socket: {error}"))?;
    let mut transcriber = Transcriber::start()?;
    let _native_shortcuts = shortcuts::ShortcutManager::start();
    #[cfg(target_os = "linux")]
    portal::start_shortcut_listener();
    let mut state = DictationState::Idle;
    let mut suspend_detector = SuspendDetector::new();
    let mut last_lock_check = Instant::now() - Duration::from_secs(1);

    let max_recording = Duration::from_millis(
        env::var("GIGATYPE_MAX_RECORDING_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(120_000),
    );
    let debounce = Duration::from_millis(
        env::var("GIGATYPE_DEBOUNCE_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(300),
    );

    loop {
        recover_disconnected_microphone(&mut state);
        let resumed = suspend_detector.resumed();
        let should_check_lock = matches!(state, DictationState::Recording { .. })
            && last_lock_check.elapsed() >= Duration::from_millis(250);
        let locked = should_check_lock && session_is_locked();
        if should_check_lock {
            last_lock_check = Instant::now();
        }
        if resumed || locked {
            discard_recording(
                &mut state,
                (
                    "Cancelled",
                    "Recording removed because the session became inactive",
                ),
            );
        }

        let timed_out = matches!(
            &state,
            DictationState::Recording { started_at, .. }
                if started_at.elapsed() >= max_recording
        );
        if timed_out {
            if let DictationState::Recording {
                recorder,
                audio_path,
                media,
                ..
            } = std::mem::replace(&mut state, DictationState::Idle)
            {
                if let Err(error) = finish_dictation(&mut transcriber, recorder, &audio_path, media)
                {
                    notify("GigaType error", &error);
                }
            }
        }

        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut command = String::new();
        BufReader::new(stream.try_clone().map_err(|error| error.to_string())?)
            .read_line(&mut command)
            .map_err(|error| error.to_string())?;
        let command = command.trim();
        let response = match command {
            "status" => Ok(match state {
                DictationState::Idle => "idle".into(),
                DictationState::Recording { .. } => "recording".into(),
            }),
            "cancel" => Ok(
                if discard_recording(&mut state, ("Cancelled", "Recording discarded")) {
                    "cancelled".into()
                } else {
                    "idle".into()
                },
            ),
            "toggle" => match std::mem::replace(&mut state, DictationState::Idle) {
                DictationState::Idle => match start_recording() {
                    Ok(recording) => {
                        state = recording;
                        eprintln!("GigaType state: idle -> recording");
                        Ok("recording".into())
                    }
                    Err(error) => Err(error),
                },
                DictationState::Recording {
                    recorder,
                    audio_path,
                    started_at,
                    media,
                    can_fallback_microphone,
                } => {
                    if started_at.elapsed() < debounce {
                        state = DictationState::Recording {
                            recorder,
                            audio_path,
                            started_at,
                            media,
                            can_fallback_microphone,
                        };
                        Ok("recording".into())
                    } else {
                        finish_dictation(&mut transcriber, recorder, &audio_path, media)
                    }
                }
            },
            "quit" => {
                writeln!(stream, "bye").ok();
                break;
            }
            _ => Err(format!("unknown daemon command: {command}")),
        };
        match response {
            Ok(message) => {
                writeln!(stream, "{message}").ok();
            }
            Err(error) => {
                notify("GigaType error", &error);
                writeln!(stream, "error: {error}").ok();
            }
        }
    }
    let _ = fs::remove_file(path);
    Ok(())
}

fn send_daemon(command: &str) -> Result<String, String> {
    if matches!(command, "toggle" | "cancel" | "status") && transcribing_path().is_file() {
        eprintln!("GigaType command: {command} ignored while transcribing");
        return Ok("transcribing".into());
    }
    let mut stream = UnixStream::connect(socket_path())
        .map_err(|_| "GigaType service is not running; run scripts/install.sh first".to_string())?;
    writeln!(stream, "{command}").map_err(|error| error.to_string())?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|error| error.to_string())?;
    let response = response.trim_end().to_string();
    if let Some(error) = response.strip_prefix("error: ") {
        Err(error.into())
    } else {
        Ok(response)
    }
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print!("{HELP}");
        return;
    }
    if args.first().map(String::as_str) == Some("postprocess") {
        println!("{}", postprocess(&args[1..].join(" ")));
        return;
    }
    if args.first().map(String::as_str) == Some("transcribe-file") {
        let Some(path) = args.get(1) else {
            eprintln!("transcribe-file requires an audio path");
            std::process::exit(2);
        };
        let result = Transcriber::start()
            .and_then(|mut transcriber| transcriber.transcribe(Path::new(path)));
        match result {
            Ok(text) => println!("{}", postprocess(&text)),
            Err(error) => {
                eprintln!("GigaType: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args[0] == "daemon" {
        if let Err(error) = daemon() {
            eprintln!("GigaType: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args[0] == "insert" {
        if let Err(error) = insert_text(&args[1..].join(" ")) {
            eprintln!("GigaType: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args[0] == "doctor" {
        doctor();
        return;
    }
    if args[0] == "microphones" {
        match audio::microphone_report() {
            Ok(report) => print!("{report}"),
            Err(error) => {
                eprintln!("GigaType: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if args[0] == "platform" {
        print!("{}", platform::Platform::detect().report());
        return;
    }
    if args[0] == "authorize" {
        #[cfg(target_os = "linux")]
        {
            if let Err(error) = portal::authorize_input() {
                eprintln!("GigaType: {error}");
                std::process::exit(1);
            }
            println!("Wayland portal input access granted");
            return;
        }
        #[cfg(not(target_os = "linux"))]
        {
            println!("No portal permission is needed on this platform");
            return;
        }
    }
    if matches!(args[0].as_str(), "toggle" | "cancel" | "status" | "quit") {
        match send_daemon(&args[0]) {
            Ok(response) => println!("{response}"),
            Err(error) => {
                eprintln!("GigaType: {error}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&16_000_u32.to_le_bytes());
        bytes.extend_from_slice(&32_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn silent_audio_is_not_sent_to_the_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), wav(&vec![0; 16_000])).unwrap();
        assert!(!audio_has_speech(file.path()).unwrap());
    }

    #[test]
    fn audible_audio_is_sent_to_the_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let samples = (0..16_000)
            .map(|index| if index % 2 == 0 { 1200 } else { -1200 })
            .collect::<Vec<_>>();
        fs::write(file.path(), wav(&samples)).unwrap();
        assert!(audio_has_speech(file.path()).unwrap());
    }

    #[test]
    fn quiet_speech_with_long_pauses_is_sent_to_the_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut samples = vec![0; 16_000 * 16];
        for (index, sample) in samples[80_000..84_800].iter_mut().enumerate() {
            *sample = if index % 2 == 0 { 400 } else { -400 };
        }
        fs::write(file.path(), wav(&samples)).unwrap();
        assert!(audio_has_speech(file.path()).unwrap());
    }

    #[test]
    fn an_isolated_click_is_not_sent_to_the_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut samples = vec![0; 16_000];
        samples[8_000] = 10_000;
        fs::write(file.path(), wav(&samples)).unwrap();
        assert!(!audio_has_speech(file.path()).unwrap());
    }

    #[test]
    fn separated_noise_bursts_are_not_sent_to_the_model() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut samples = vec![0; 16_000];
        for start in [8_000, 12_800] {
            for (index, sample) in samples[start..start + 80].iter_mut().enumerate() {
                *sample = if index % 2 == 0 { 2_000 } else { -2_000 };
            }
        }
        fs::write(file.path(), wav(&samples)).unwrap();
        assert!(!audio_has_speech(file.path()).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn kde_wayland_uses_uinput_even_when_xwayland_sets_display() {
        assert_eq!(
            linux_keyboard_route(true, true, "KDE", false),
            LinuxKeyboardRoute::Uinput
        );
    }
}
