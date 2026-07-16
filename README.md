# GigaType

[![CI](https://github.com/kpoxo6op/gigatype/actions/workflows/ci.yml/badge.svg)](https://github.com/kpoxo6op/gigatype/actions/workflows/ci.yml)

GigaType is a private, local Russian dictation app for KDE Plasma on Linux. Press `F9`, speak, press `F9` again, and the transcription is inserted into the focused app. Audio and text stay on the computer.

It uses a fast Rust desktop daemon and keeps [GigaAM Multilingual](https://huggingface.co/ai-sage/GigaAM-Multilingual) warm in a persistent Python worker. This avoids reloading roughly 900 MB of model weights for every sentence.

## MVP features

- system-wide push-to-talk toggle (`F9` by default) and safe cancel shortcut (`Shift+F9`)
- short, distinct audio cues for “listening” and “transcribing”
- direct insertion at the current cursor on KDE Wayland
- lossless clipboard preservation, including images, files, HTML, and other MIME types
- configurable `Ctrl+V`, `Ctrl+Shift+V`, or `Shift+Insert` paste for GUI apps and terminals, with copy-only fallback when injection is unavailable
- Russian spoken punctuation: `точка`, `запятая`, `вопросительный знак`, `восклицательный знак`, `двоеточие`, `точка с запятой`, `многоточие`, `новая строка`
- automatic whitespace cleanup, sentence capitalization, and terminal punctuation
- persistent local GigaAM worker and recordings longer than 25 seconds split safely
- silence rejection, preferred-microphone fallback, and automatic media pause/resume
- double-press protection, a two-minute recording ceiling, and private-audio cleanup on lock, suspend, stop, or restart
- desktop notifications, cancel/status commands, transcript history, and a health check
- automatic local model setup; no account or API token required
- no network access during transcription

## Requirements

The current MVP targets KDE Plasma 6 on Wayland and Debian/Ubuntu-style Linux distributions. It needs approximately 2 GB for the CPU model and its isolated Python environment.

Install the system packages first:

```bash
sudo apt install cargo rustfmt ffmpeg pipewire-bin qdbus-qt6 \
  libglib2.0-bin libnotify-bin python3 python3-venv
```

## Install

Clone and run the installer:

```bash
git clone https://github.com/kpoxo6op/gigatype.git
cd gigatype
./scripts/install.sh
```

On the first run, the installer creates an isolated Python environment and downloads the public `ctc` revision of GigaAM Multilingual. It then builds the release binary, starts a user service, registers `F9` only if the shortcut is free, and grants the installing user access to a narrowly scoped virtual keyboard device.

To inspect the model setup without changing anything:

```bash
./scripts/bootstrap-model.sh --print-plan
```

## Use

Press `F9` once to listen and again to stop, transcribe, and insert. Press `Shift+F9` to cancel and delete the current recording without transcribing it. Useful commands:

```bash
gigatype toggle
gigatype cancel
gigatype status
gigatype doctor
gigatype transcribe-file recording.wav
```

History is stored at `~/.local/share/gigatype/history.jsonl`. Temporary microphone audio lives in the per-login runtime directory and is deleted immediately after every transcription attempt. Locking the desktop, suspending the laptop, cancelling, or stopping/restarting the service also removes an unfinished recording.

The cues use the desktop's short FreeDesktop device-added/device-removed sounds. To turn them off, add `Environment=GIGATYPE_NO_SOUNDS=1` to a systemd user-service override. Custom `.oga`, `.ogg`, or `.wav` files can be selected with `GIGATYPE_START_SOUND` and `GIGATYPE_STOP_SOUND`.

### Reliability settings

Settings are environment variables in a systemd user override (`systemctl --user edit gigatype.service`). Defaults work for normal KDE applications:

| Setting | Default | Purpose |
| --- | --- | --- |
| `GIGATYPE_PASTE_KEYS` | `ctrl+v` | Use `ctrl+shift+v` or `shift+insert` for a terminal-first setup |
| `GIGATYPE_CLIPBOARD_RESTORE_MS` | `120` | Wait before restoring every original clipboard format |
| `GIGATYPE_MAX_RECORDING_MS` | `120000` | Stop and transcribe automatically at the duration limit |
| `GIGATYPE_DEBOUNCE_MS` | `300` | Ignore accidental duplicate shortcut presses |
| `GIGATYPE_MICROPHONE` | system default | Preferred PipeWire node name or serial; falls back safely if unavailable or disconnected |
| `GIGATYPE_SILENCE_RMS` | `80` | PCM energy threshold below which silent recordings skip transcription |
| `GIGATYPE_KEEP_MEDIA_PLAYING` | unset | Set to `1` to disable MPRIS pause/resume |

After changing an override, run `systemctl --user daemon-reload && systemctl --user restart gigatype.service`.

## Remove the app

```bash
./scripts/uninstall.sh
```

The uninstall script deliberately keeps the separately installed GigaAM model.

## Privacy and security

- Transcription runs locally. The systemd service forces Hugging Face offline mode after installation.
- Installation downloads Python packages and model files from PyPI, the official PyTorch CPU index, and Hugging Face. Normal dictation does not use the network.
- Raw microphone audio is deleted immediately after each transcription attempt or cancellation.
- Transcript history is plain JSON Lines stored with owner-only `0600` permissions. Delete it at any time if you do not want local history.
- Direct insertion requires `/dev/uinput`. The installer adds a udev rule that grants only the installing user `0600` access, and the uninstaller removes that rule. Anyone able to replace the process running as that user could synthesize keyboard input, so review the installer before using it on a shared machine.
- The model weights are downloaded during installation and are not distributed in this repository.

## Scope

This is an early Linux/KDE MVP. Packaging for other distributions, a settings UI, tray controls, non-KDE desktop support, and GPU/ONNX backends are future work rather than release blockers.

## Architecture

`gigatype daemon` owns a Unix control socket and the microphone state. `worker/gigaam_worker.py` loads GigaAM once and accepts one JSON request per line. The Rust process handles audio capture, formatting, KDE clipboard preservation, virtual-keyboard paste, notifications, and history.

## License

GigaType is released under the [MIT License](LICENSE). GigaAM Multilingual is a separate MIT-licensed model downloaded from its publisher at installation time.
