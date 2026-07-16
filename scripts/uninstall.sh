#!/usr/bin/env bash
set -euo pipefail

systemctl --user disable --now gigatype.service 2>/dev/null || true
rm -f "${HOME}/.config/systemd/user/gigatype.service"
rm -f "${HOME}/.local/share/applications/io.github.gigatype.desktop"
rm -f "${HOME}/.local/bin/gigatype"
rm -rf "${HOME}/.local/share/gigatype"
sudo rm -f /etc/udev/rules.d/99-gigatype-uinput.rules
sudo udevadm control --reload-rules
systemctl --user daemon-reload
if command -v kbuildsycoca6 >/dev/null 2>&1; then
  kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
fi
echo "GigaType was removed. The GigaAM model was kept."
