use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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
    },
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

fn start_recording() -> Result<DictationState, String> {
    let audio_path = runtime_dir().join("gigatype-recording.wav");
    let _ = fs::remove_file(&audio_path);
    play_sound(SoundCue::Listening);
    let child = if env::var_os("GIGATYPE_FAKE_RECORDING").is_some() {
        fs::write(&audio_path, b"fake audio").map_err(|error| error.to_string())?;
        None
    } else {
        Some(
            Command::new("pw-record")
                .args(["--rate", "16000", "--channels", "1", "--format", "s16"])
                .arg(&audio_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| format!("could not start microphone recording: {error}"))?,
        )
    };
    notify("Listening…", "Press the shortcut again to transcribe");
    Ok(DictationState::Recording { child, audio_path })
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

fn get_clipboard() -> Result<String, String> {
    let output = Command::new("qdbus6")
        .args([
            "org.kde.klipper",
            "/klipper",
            "org.kde.klipper.klipper.getClipboardContents",
        ])
        .output()
        .map_err(|error| format!("could not read KDE clipboard: {error}"))?;
    if !output.status.success() {
        return Err("could not read KDE clipboard".into());
    }
    let mut text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    if text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
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

fn paste_shortcut(probe_only: bool) -> Result<(), String> {
    const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
    const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
    const UI_DEV_CREATE: libc::c_ulong = 0x5501;
    const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
    const KEY_LEFTCTRL: u16 = 29;
    const KEY_V: u16 = 47;

    let mut device = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/uinput")
        .map_err(|error| format!("cannot type into apps via /dev/uinput: {error}"))?;
    let fd = device.as_raw_fd();
    for (request, value) in [
        (UI_SET_EVBIT, 1),
        (UI_SET_KEYBIT, KEY_LEFTCTRL as libc::c_int),
        (UI_SET_KEYBIT, KEY_V as libc::c_int),
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
    for (code, value) in [(KEY_LEFTCTRL, 1), (KEY_V, 1), (KEY_V, 0), (KEY_LEFTCTRL, 0)] {
        emit_key(&mut device, code, value)?;
        emit_sync(&mut device)?;
    }
    thread::sleep(Duration::from_millis(80));
    unsafe { libc::ioctl(fd, UI_DEV_DESTROY) };
    Ok(())
}

fn insert_text(text: &str) -> Result<(), String> {
    if let Some(path) = env::var_os("GIGATYPE_FAKE_INSERT_LOG") {
        return fs::write(path, text).map_err(|error| error.to_string());
    }
    let previous = get_clipboard()?;
    put_on_clipboard(text)?;
    let result = paste_shortcut(false);
    thread::sleep(Duration::from_millis(120));
    let restore = put_on_clipboard(&previous);
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
) -> Result<String, String> {
    stop_recording(&mut child, audio_path)?;
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

fn daemon() -> Result<(), String> {
    fs::create_dir_all(runtime_dir()).map_err(|error| error.to_string())?;
    let path = socket_path();
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .map_err(|error| format!("could not bind {}: {error}", path.display()))?;
    let mut worker = Worker::start()?;
    let mut state = DictationState::Idle;

    for incoming in listener.incoming() {
        let mut stream = incoming.map_err(|error| error.to_string())?;
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
            "cancel" => match std::mem::replace(&mut state, DictationState::Idle) {
                DictationState::Idle => Ok("idle".into()),
                DictationState::Recording {
                    mut child,
                    audio_path,
                } => {
                    if let Some(process) = &mut child {
                        let _ = process.kill();
                        let _ = process.wait();
                    }
                    let _ = fs::remove_file(audio_path);
                    notify("Cancelled", "Recording discarded");
                    Ok("cancelled".into())
                }
            },
            "toggle" => match std::mem::replace(&mut state, DictationState::Idle) {
                DictationState::Idle => match start_recording() {
                    Ok(recording) => {
                        state = recording;
                        Ok("recording".into())
                    }
                    Err(error) => Err(error),
                },
                DictationState::Recording { child, audio_path } => {
                    finish_dictation(&mut worker, child, &audio_path)
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
