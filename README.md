# GigaType

[![CI](https://github.com/kpoxo6op/gigatype/actions/workflows/ci.yml/badge.svg)](https://github.com/kpoxo6op/gigatype/actions/workflows/ci.yml)

GigaType is local Russian voice typing for Unix desktops. Press `F9`, speak, then press `F9` again to insert the transcription into the focused app.

It runs [GigaAM v3](https://huggingface.co/ai-sage/GigaAM-v3) on your computer. Dictation does not use a cloud API, account, token, or Python runtime.

## Install

```bash
git clone https://github.com/kpoxo6op/gigatype.git
cd gigatype
./scripts/install.sh
```

The installer downloads the checked model files, builds GigaType, configures autostart, and requests any desktop permissions it needs.

GigaType supports Linux on Wayland or X11, macOS, and BSD desktops running X11. Run this after installation to check your setup:

```bash
gigatype doctor
```

## Use

- `F9`: start listening; press again to stop, transcribe, and insert
- `Shift+F9`: cancel the current recording

Useful commands:

```bash
gigatype status
gigatype microphones
gigatype transcribe-file recording.wav
gigatype authorize
```

On macOS, allow GigaType access to the microphone and Accessibility controls when prompted. Wayland permission is handled by `gigatype authorize` and the installer.

## Local data

- Model: `~/.local/share/gigatype/models/`
- Transcript history: `~/.local/share/gigatype/history.jsonl`
- Temporary audio: deleted after transcription or cancellation

The model is downloaded once. Normal dictation works offline.

## Testing

GitHub-hosted E2E tests the real GigaAM model through a virtual microphone, checks that the beginning and end of the recording are preserved, inserts the result into a focused X11 editor, and verifies clipboard restoration.

CI also holds a fake clipboard transfer open indefinitely and verifies that GigaType times out, inserts without preserving that clipboard, returns to idle, and accepts the next `F9`.

It does not test a physical microphone or KDE Wayland. Those require an interactive machine.

## Remove

```bash
./scripts/uninstall.sh
```

The uninstaller removes GigaType and its local services. It keeps the downloaded model so it does not need to be downloaded again if you reinstall.
