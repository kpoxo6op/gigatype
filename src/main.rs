use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use wl_clipboard_rs::{copy, paste};

const HELP: &str = "GigaType — local Russian voice typing with GigaAM

Usage: gigatype <command>

Commands:
  toggle       Start speaking, or stop and insert the transcription
  cancel       Discard the current recording
  status       Show whether GigaType is idle, recording, or transcribing
  daemon       Run the background service
  doctor       Check audio, model, desktop, and text-insertion support
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

#[derive(Serialize)]
struct WorkerRequest<'a> {
    id: u64,
    audio_path: &'a Path,
}

#[derive(Deserialize)]
struct WorkerReply {
    text: Option<String>,
    error: Option<String>,
}

struct Worker {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Worker {
    fn start() -> Result<Self, String> {
        let python = env::var_os("GIGATYPE_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs_home().join(".local/share/russian-asr/venv/bin/python"));
        let installed_worker = dirs_home().join(".local/share/gigatype/gigaam_worker.py");
        let worker = env::var_os("GIGATYPE_WORKER")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if installed_worker.is_file() {
                    installed_worker
                } else {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("worker/gigaam_worker.py")
                }
            });
        let mut child = Command::new(&python)
            .arg(&worker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", worker.display()))?;
        let stdin = child.stdin.take().ok_or("worker stdin is unavailable")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("worker stdout is unavailable")?);
        Ok(Self {
            _child: child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    fn transcribe(&mut self, audio_path: &Path) -> Result<String, String> {
        let id = self.next_id;
        self.next_id += 1;
        serde_json::to_writer(&mut self.stdin, &WorkerRequest { id, audio_path })
            .map_err(|error| error.to_string())?;
        self.stdin
            .write_all(b"\n")
            .map_err(|error| error.to_string())?;
        self.stdin.flush().map_err(|error| error.to_string())?;

        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if line.is_empty() {
            return Err("GigaAM worker exited without a reply".into());
        }
        let reply: WorkerReply = serde_json::from_str(&line)
            .map_err(|error| format!("invalid reply from GigaAM worker: {error}"))?;
        match (reply.text, reply.error) {
            (Some(text), _) => Ok(text),
            (_, Some(error)) => Err(error),
            _ => Err("GigaAM worker returned an empty reply".into()),
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

enum DictationState {
    Idle,
    Recording {
        child: Option<Child>,
        audio_path: PathBuf,
        started_at: Instant,
        media: MediaGuard,
        can_fallback_microphone: bool,
    },
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
    if let Some(log) = env::var_os("GIGATYPE_SOUND_LOG") {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(file, "{label}");
        }
        return;
    }
    let sound = env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback));
    if sound.is_file() {
        let _ = Command::new("pw-play")
            .arg(sound)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn notify(summary: &str, body: &str) {
    if env::var_os("GIGATYPE_NO_NOTIFY").is_some() {
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
    let (child, can_fallback_microphone) = if env::var_os("GIGATYPE_FAKE_RECORDING").is_some() {
        fs::write(&audio_path, b"fake audio").map_err(|error| error.to_string())?;
        (None, false)
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
                (Some(process), false)
            } else {
                (Some(process), true)
            }
        } else {
            (Some(process), false)
        }
    };
    notify("Listening…", "Press the shortcut again to transcribe");
    Ok(DictationState::Recording {
        child,
        audio_path,
        started_at: Instant::now(),
        media,
        can_fallback_microphone,
    })
}

fn recover_disconnected_microphone(state: &mut DictationState) {
    let DictationState::Recording {
        child: Some(process),
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

fn stop_recording(child: &mut Option<Child>, audio_path: &Path) -> Result<(), String> {
    if let Some(process) = child {
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
    for sample in data.chunks_exact(2) {
        let value = i16::from_le_bytes([sample[0], sample[1]]) as f64;
        squared += value * value;
        if value.abs() >= threshold * 4.0 {
            loud_samples += 1;
        }
        sample_count += 1;
    }
    let rms = (squared / sample_count as f64).sqrt();
    Ok(rms >= threshold && loud_samples * 200 >= sample_count)
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
        .status()
        .map_err(|error| format!("could not call KDE clipboard: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("KDE clipboard rejected the transcript".into())
    }
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

fn capture_clipboard() -> Result<ClipboardSnapshot, String> {
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
        let mut data = Vec::new();
        pipe.read_to_end(&mut data)
            .map_err(|error| format!("could not preserve clipboard type {mime_type}: {error}"))?;
        entries.push(ClipboardEntry { mime_type, data });
    }
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
    copy::copy_multi(options, sources)
        .map_err(|error| format!("could not restore the Wayland clipboard: {error}"))
}

fn set_transcript_clipboard(text: &str) -> Result<(), String> {
    if env::var_os("GIGATYPE_FAKE_CLIPBOARD_LOG").is_some() {
        append_fake_clipboard_log("set:text/plain;charset=utf-8")?;
        return Ok(());
    }
    put_on_clipboard(text)
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
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
struct InputEvent {
    time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

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
    const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
    const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
    const UI_DEV_CREATE: libc::c_ulong = 0x5501;
    const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
    const KEY_LEFTCTRL: u16 = 29;
    const KEY_LEFTSHIFT: u16 = 42;
    const KEY_V: u16 = 47;
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

fn insert_text(text: &str) -> Result<(), String> {
    if env::var_os("GIGATYPE_FAKE_CLIPBOARD_LOG").is_none() {
        if let Some(path) = env::var_os("GIGATYPE_FAKE_INSERT_LOG") {
            return fs::write(path, text).map_err(|error| error.to_string());
        }
    }
    let previous = capture_clipboard()?;
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
    let restore = restore_clipboard(previous);
    result.and(restore)
}

fn command_exists(name: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

fn doctor() {
    let model = env::var_os("GIGATYPE_MODEL")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_home().join(".local/share/russian-asr/gigaam-multilingual-ctc"));
    let model_ok = model.join("pytorch_model.bin").is_file();
    println!(
        "{} GigaAM model       {}",
        if model_ok { "✓" } else { "✗" },
        model.display()
    );

    let recorder_ok = command_exists("pw-record");
    println!(
        "{} Microphone recorder pw-record",
        if recorder_ok { "✓" } else { "✗" }
    );

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
        "{} KDE clipboard       Klipper D-Bus",
        if clipboard_ok { "✓" } else { "✗" }
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
    worker: &mut Worker,
    mut child: Option<Child>,
    audio_path: &Path,
    media: MediaGuard,
) -> Result<String, String> {
    let stopped = stop_recording(&mut child, audio_path);
    media.resume();
    stopped?;
    if env::var_os("GIGATYPE_FAKE_RECORDING").is_none() && !audio_has_speech(audio_path)? {
        let _ = fs::remove_file(audio_path);
        notify(
            "No speech detected",
            "Recording was silent and was not transcribed",
        );
        return Err("no speech was detected".into());
    }
    play_sound(SoundCue::Transcribing);
    notify("Transcribing…", "GigaAM is processing your speech locally");
    let transcription = worker.transcribe(audio_path);
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
            notify(
                "Text copied",
                &format!("Auto-insert unavailable ({error}). Press Ctrl+V."),
            );
        } else {
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
        mut child,
        audio_path,
        media,
        ..
    } = std::mem::replace(state, DictationState::Idle)
    else {
        return false;
    };
    if let Some(process) = &mut child {
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
    let path = socket_path();
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .map_err(|error| format!("could not bind {}: {error}", path.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure daemon socket: {error}"))?;
    let mut worker = Worker::start()?;
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
                child,
                audio_path,
                media,
                ..
            } = std::mem::replace(&mut state, DictationState::Idle)
            {
                if let Err(error) = finish_dictation(&mut worker, child, &audio_path, media) {
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
                        Ok("recording".into())
                    }
                    Err(error) => Err(error),
                },
                DictationState::Recording {
                    child,
                    audio_path,
                    started_at,
                    media,
                    can_fallback_microphone,
                } => {
                    if started_at.elapsed() < debounce {
                        state = DictationState::Recording {
                            child,
                            audio_path,
                            started_at,
                            media,
                            can_fallback_microphone,
                        };
                        Ok("recording".into())
                    } else {
                        finish_dictation(&mut worker, child, &audio_path, media)
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
        let result = Worker::start().and_then(|mut worker| worker.transcribe(Path::new(path)));
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
}
