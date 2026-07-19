#!/usr/bin/env bash
set -euo pipefail

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user disable --now gigatype.service 2>/dev/null || true
  rm -f "${HOME}/.config/systemd/user/gigatype.service"
  systemctl --user daemon-reload
fi
if command -v launchctl >/dev/null 2>&1; then
  launchctl bootout "gui/$(id -u)/io.github.gigatype" 2>/dev/null || true
fi
rm -f "${HOME}/Library/LaunchAgents/io.github.gigatype.plist"
rm -f "${HOME}/.config/autostart/io.github.gigatype.autostart.desktop"
rm -f "${HOME}/.local/share/applications/io.github.gigatype.desktop"
rm -f "${HOME}/.local/share/applications/io.github.gigatype.cancel.desktop"
rm -f "${HOME}/.local/bin/gigatype"
rm -f "${HOME}/.local/share/gigatype/history.jsonl"
rm -f "${HOME}/.local/share/gigatype/daemon.log"
rm -f "${HOME}/.local/share/gigatype/portal-input-token"

if [[ -f /etc/udev/rules.d/99-gigatype-uinput.rules ]] && command -v sudo >/dev/null 2>&1; then
  sudo rm -f /etc/udev/rules.d/99-gigatype-uinput.rules
  sudo udevadm control --reload-rules
fi

echo "GigaType was removed. The GigaAM model was kept in ~/.local/share/gigatype/models/."
