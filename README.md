# GigaType

[![CI](https://github.com/kpoxo6op/gigatype/actions/workflows/ci.yml/badge.svg)](https://github.com/kpoxo6op/gigatype/actions/workflows/ci.yml)

GigaType is private, local Russian voice typing for Unix desktops. Press `F9`, speak, press `F9` again, and the transcription is inserted into the focused app. Audio and text stay on your computer.

Version 0.3 runs the punctuation-aware [GigaAM v3 end-to-end RNN-T](https://huggingface.co/ai-sage/GigaAM-v3) model directly from Rust through ONNX Runtime. There is no Python environment, background model script, cloud API, account, or token.

## What it does

- global `F9` start/stop and `Shift+F9` cancel shortcuts
- native CPAL microphone recording and pleasant start/stop chimes
- Russian punctuation, capitalization, number formatting, and normalization from GigaAM v3
- direct insertion at the current cursor with clipboard restoration
- XDG GlobalShortcuts and RemoteDesktop portals on Wayland
- native X11 and macOS shortcut/insertion fallbacks; Linux `/dev/uinput` as a last resort
- automatic system-default microphone fallback
- silence rejection, double-press protection, and a two-minute safety ceiling
- optional media pause/resume while listening
- private temporary-audio cleanup on completion, cancellation, lock, suspend, stop, or restart
- local transcript history with owner-only permissions
- checked model downloads and fully offline transcription

## Supported systems

| System | Audio | Shortcut and insertion | Autostart |
| --- | --- | --- | --- |
| Linux Wayland | CPAL/PipeWire or PulseAudio; ALSA fallback | XDG desktop portals; KDE GlobalAccel/uinput fallback | systemd user service |
| Linux X11 | CPAL/PipeWire or PulseAudio; ALSA fallback | native X11 | systemd user service |
| macOS | CPAL/CoreAudio | native macOS events | launchd |
| FreeBSD/OpenBSD/NetBSD/DragonFly X11 | CPAL host backend | native X11 | XDG autostart |

The release pipeline builds Linux x86-64 and both Intel and Apple Silicon macOS archives. The repository also contains AppImage, Debian, RPM, Nix, and Homebrew packaging.

## Install

Clone the repository and run the installer:

```bash
git clone https://github.com/kpoxo6op/gigatype.git
cd gigatype
./scripts/install.sh
```

The installer detects `apt`, `dnf`, `pacman`, FreeBSD `pkg`, or Homebrew and installs missing native build tools. It downloads the four checked GigaAM ONNX assets to `~/.local/share/gigatype/models/`, builds GigaType, configures autostart, and asks for the desktop permission needed to type at the cursor.

To inspect the model download without changing the computer:

```bash
./scripts/bootstrap-model.sh --print-plan
```

Distribution packages can also be built from:

- `packaging/appimage/`
- `packaging/debian/`
- `packaging/rpm/`
- `flake.nix`
- `packaging/homebrew/gigatype.rb`

## Use

Press `F9` once to listen and again to stop, transcribe, and insert. Press `Shift+F9` to cancel and delete the current recording.

```bash
gigatype toggle
gigatype cancel
gigatype status
gigatype doctor
gigatype microphones
gigatype platform
gigatype transcribe-file recording.wav
```

History is stored at `~/.local/share/gigatype/history.jsonl`. Temporary microphone audio lives in the per-login runtime directory and is deleted immediately after each attempt.

### Settings

These environment variables may be placed in a systemd user override, launchd plist, or desktop autostart entry:

| Setting | Default | Purpose |
| --- | --- | --- |
| `GIGATYPE_MODEL` | `~/.local/share/gigatype/models/gigaam-v3-e2e-rnnt` | Model asset directory |
| `GIGATYPE_PASTE_KEYS` | `ctrl+v` | `ctrl+v`, `ctrl+shift+v`, or `shift+insert` |
| `GIGATYPE_CLIPBOARD_RESTORE_MS` | `120` | Delay before restoring the previous clipboard |
| `GIGATYPE_MAX_RECORDING_MS` | `120000` | Automatic stop and transcription limit |
| `GIGATYPE_DEBOUNCE_MS` | `300` | Ignore accidental duplicate shortcut presses |
| `GIGATYPE_MICROPHONE` | system default | Preferred microphone name |
| `GIGATYPE_SILENCE_RMS` | `80` | PCM silence threshold |
| `GIGATYPE_KEEP_MEDIA_PLAYING` | unset | Disable MPRIS pause/resume when set |
| `GIGATYPE_NO_SOUNDS` | unset | Disable the native listening/transcribing chimes |
| `GIGATYPE_START_SOUND` | native chime | Custom sound file played by the platform player |
| `GIGATYPE_STOP_SOUND` | native chime | Custom sound file played by the platform player |

## Desktop permissions

On Wayland, `gigatype authorize` requests persistent keyboard permission through the standard XDG RemoteDesktop portal. Global shortcuts are registered through the XDG GlobalShortcuts portal. KDE uses its mature GlobalAccel integration and the narrowly scoped uinput fallback because its RemoteDesktop portal can block unattended background services; other compositors fall back the same way if their portals are incomplete.

On macOS, approve GigaType under **System Settings → Privacy & Security → Microphone** and **Accessibility** the first time it asks.

Run `gigatype doctor` to see the adapter selected for every runtime layer.

## Privacy and security

- Normal dictation makes no network requests.
- The model download is verified against committed SHA-256 checksums.
- Raw microphone audio is deleted after each attempt or cancellation.
- Transcript history is JSON Lines with `0600` permissions.
- The portal is preferred because the desktop owns and displays its permissions.
- The Linux uinput rule is installed only when portal keyboard access fails, grants only the installing user access, and is removed by the uninstaller.

## Architecture

`gigatype daemon` owns a Unix control socket, the CPAL audio stream, three persistent ONNX sessions (encoder, RNN-T predictor, and joint network), desktop adapters, and dictation state. Rust implements the exact 16 kHz log-mel frontend used by GigaAM and SentencePiece decoding. Recordings over 25 seconds are processed in safe 22-second chunks.

The model weights are downloaded separately and are not committed to this repository. GigaType is MIT licensed; the GigaAM model is distributed separately by its publisher under its own MIT license.

## Remove

```bash
./scripts/uninstall.sh
```

The app, services, permissions, history, and logs are removed. The downloaded GigaAM model is deliberately kept.
