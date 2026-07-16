#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
BIN_DIR=${HOME}/.local/bin
DATA_DIR=${HOME}/.local/share/gigatype
APP_DIR=${HOME}/.local/share/applications
SERVICE_DIR=${HOME}/.config/systemd/user
MODEL=${HOME}/.local/share/russian-asr/gigaam-multilingual-ctc/pytorch_model.bin
PYTHON=${HOME}/.local/share/russian-asr/venv/bin/python

missing=()
for command in cargo ffmpeg ffprobe pw-record pw-play qdbus6 gdbus notify-send python3 sudo systemctl udevadm; do
  command -v "$command" >/dev/null 2>&1 || missing+=("$command")
done
if (( ${#missing[@]} )); then
  echo "Missing required commands: ${missing[*]}" >&2
  echo "Debian/Ubuntu packages: cargo rustfmt ffmpeg pipewire-bin qdbus-qt6 libglib2.0-bin libnotify-bin python3 python3-venv" >&2
  exit 1
fi

if [[ ! -f "$MODEL" || ! -x "$PYTHON" ]]; then
  "$ROOT/scripts/bootstrap-model.sh"
fi

cd "$ROOT"
cargo build --release

install -d "$BIN_DIR" "$DATA_DIR" "$APP_DIR" "$SERVICE_DIR"
install -m 0755 target/release/gigatype "$BIN_DIR/gigatype"
install -m 0755 worker/gigaam_worker.py "$DATA_DIR/gigaam_worker.py"
install -m 0644 packaging/systemd/gigatype.service "$SERVICE_DIR/gigatype.service"
[[ -f "$DATA_DIR/history.jsonl" ]] && chmod 0600 "$DATA_DIR/history.jsonl"

shortcut_available=true
if command -v qdbus6 >/dev/null 2>&1; then
  shortcut_available=$(qdbus6 org.kde.kglobalaccel /kglobalaccel \
    org.kde.KGlobalAccel.isGlobalShortcutAvailable 16777272 io.github.gigatype.desktop 2>/dev/null || true)
  shortcut_owner=$(qdbus6 org.kde.kglobalaccel /kglobalaccel \
    org.kde.KGlobalAccel.action 16777272 2>/dev/null | head -n 1 || true)
  if [[ "$shortcut_owner" == "io.github.gigatype.desktop" ]]; then
    shortcut_available=true
  fi
fi
if [[ "$shortcut_available" == "true" ]]; then
  install -m 0644 packaging/applications/io.github.gigatype.desktop \
    "$APP_DIR/io.github.gigatype.desktop"
else
  sed '/^X-KDE-Shortcuts=/d' packaging/applications/io.github.gigatype.desktop \
    >"$APP_DIR/io.github.gigatype.desktop"
  echo "F9 is already used, so no shortcut was taken." >&2
fi

cancel_shortcut_available=true
if command -v qdbus6 >/dev/null 2>&1; then
  cancel_shortcut_available=$(qdbus6 org.kde.kglobalaccel /kglobalaccel \
    org.kde.KGlobalAccel.isGlobalShortcutAvailable 50331704 io.github.gigatype.cancel.desktop 2>/dev/null || true)
  cancel_shortcut_owner=$(qdbus6 org.kde.kglobalaccel /kglobalaccel \
    org.kde.KGlobalAccel.action 50331704 2>/dev/null | head -n 1 || true)
  if [[ "$cancel_shortcut_owner" == "io.github.gigatype.cancel.desktop" ]]; then
    cancel_shortcut_available=true
  fi
fi
if [[ "$cancel_shortcut_available" == "true" ]]; then
  install -m 0644 packaging/applications/io.github.gigatype.cancel.desktop \
    "$APP_DIR/io.github.gigatype.cancel.desktop"
else
  sed '/^X-KDE-Shortcuts=/d' packaging/applications/io.github.gigatype.cancel.desktop \
    >"$APP_DIR/io.github.gigatype.cancel.desktop"
  echo "Shift+F9 is already used, so no cancel shortcut was taken." >&2
fi

if command -v sudo >/dev/null 2>&1; then
  rule=$(mktemp)
  trap 'rm -f "$rule"' EXIT
  sed "s/@USER@/${USER}/g" packaging/99-gigatype-uinput.rules >"$rule"
  sudo install -m 0644 "$rule" /etc/udev/rules.d/99-gigatype-uinput.rules
  sudo modprobe uinput
  sudo udevadm control --reload-rules
  sudo udevadm trigger --name-match=uinput || true
fi

if command -v kbuildsycoca6 >/dev/null 2>&1; then
  kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
fi
systemctl --user daemon-reload
systemctl --user enable gigatype.service
systemctl --user restart gigatype.service
for _ in {1..50}; do
  if "$BIN_DIR/gigatype" status >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

echo
echo "GigaType is installed. Press F9 to start/stop, or Shift+F9 to cancel."
echo "Run 'gigatype doctor' for a health check."
