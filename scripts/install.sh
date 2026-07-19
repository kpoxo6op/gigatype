#!/usr/bin/env bash
set -euo pipefail
export PATH="${HOME}/.cargo/bin:${PATH}"

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
BIN_DIR=${HOME}/.local/bin
DATA_DIR=${HOME}/.local/share/gigatype
APP_DIR=${HOME}/.local/share/applications
SYSTEMD_DIR=${HOME}/.config/systemd/user
AUTOSTART_DIR=${HOME}/.config/autostart
LAUNCH_AGENTS=${HOME}/Library/LaunchAgents

install_build_dependencies() {
  local ready=true
  for command_name in cargo cmake curl; do
    command -v "$command_name" >/dev/null 2>&1 || ready=false
  done
  case "$(uname -s)" in
    Linux)
      command -v pkg-config >/dev/null 2>&1 || ready=false
      pkg-config --exists alsa 2>/dev/null || ready=false
      ;;
  esac
  if [[ "$ready" == true ]]; then
    return
  fi
  echo "Installing native build dependencies"
  if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update
    sudo apt-get install -y cargo rustfmt build-essential cmake pkg-config libasound2-dev libssl-dev curl
  elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y cargo rustfmt gcc-c++ cmake pkgconf-pkg-config alsa-lib-devel openssl-devel curl
  elif command -v pacman >/dev/null 2>&1; then
    sudo pacman -S --needed --noconfirm rust base-devel cmake pkgconf alsa-lib openssl curl
  elif command -v pkg >/dev/null 2>&1; then
    sudo pkg install -y rust cmake pkgconf alsa-lib openssl curl
  elif command -v brew >/dev/null 2>&1; then
    brew install rust cmake pkg-config
  else
    echo "Install Rust, CMake, a C++ compiler, pkg-config, and ALSA headers, then rerun this installer." >&2
    exit 1
  fi
}

ensure_modern_rust() {
  local version
  version=$(rustc --version 2>/dev/null | awk '{print $2}' || true)
  if [[ -n "$version" && "$(printf '%s\n' 1.88.0 "$version" | sort -V | head -n1)" == 1.88.0 ]]; then
    return
  fi
  echo "Installing a current user-local Rust toolchain (GigaType requires Rust 1.88+)"
  RUSTUP_INIT_SKIP_PATH_CHECK=yes curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable
}

install_build_dependencies
ensure_modern_rust
"$ROOT/scripts/bootstrap-model.sh"

cd "$ROOT"
cargo build --locked --release

install -d "$BIN_DIR" "$DATA_DIR" "$APP_DIR"
install -m 0755 target/release/gigatype "$BIN_DIR/gigatype"
[[ -f "$DATA_DIR/history.jsonl" ]] && chmod 0600 "$DATA_DIR/history.jsonl"

rm -f "$APP_DIR/io.github.gigatype.desktop" "$APP_DIR/io.github.gigatype.cancel.desktop"

case "$(uname -s)" in
  Linux)
    if [[ "${XDG_CURRENT_DESKTOP,,}" == *kde* ]]; then
      install -m 0644 packaging/applications/io.github.gigatype.desktop \
        "$APP_DIR/io.github.gigatype.desktop"
      install -m 0644 packaging/applications/io.github.gigatype.cancel.desktop \
        "$APP_DIR/io.github.gigatype.cancel.desktop"
      if command -v kbuildsycoca6 >/dev/null 2>&1; then
        kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
      fi
    fi
    if command -v systemctl >/dev/null 2>&1; then
      install -d "$SYSTEMD_DIR"
      install -m 0644 packaging/systemd/gigatype.service "$SYSTEMD_DIR/gigatype.service"
      systemctl --user daemon-reload
      systemctl --user reenable gigatype.service
    else
      install -d "$AUTOSTART_DIR"
      install -m 0644 packaging/autostart/io.github.gigatype.autostart.desktop \
        "$AUTOSTART_DIR/io.github.gigatype.autostart.desktop"
    fi
    needs_uinput=false
    if [[ -n "${WAYLAND_DISPLAY:-}" && "${XDG_CURRENT_DESKTOP,,}" == *kde* ]]; then
      needs_uinput=true
    elif [[ -n "${WAYLAND_DISPLAY:-}" ]] && ! "$BIN_DIR/gigatype" authorize; then
      echo "The desktop portal declined keyboard access; enabling the Linux uinput fallback."
      needs_uinput=true
    fi
    if [[ "$needs_uinput" == true ]]; then
      rule=$(mktemp)
      trap 'rm -f "$rule"' EXIT
      sed "s/@USER@/${USER}/g" packaging/99-gigatype-uinput.rules >"$rule"
      sudo install -m 0644 "$rule" /etc/udev/rules.d/99-gigatype-uinput.rules
      sudo modprobe uinput
      sudo udevadm control --reload-rules
      sudo udevadm trigger --name-match=uinput || true
    fi
    if command -v systemctl >/dev/null 2>&1; then
      systemctl --user restart gigatype.service
    fi
    ;;
  Darwin)
    install -d "$LAUNCH_AGENTS"
    sed "s|@HOME@|${HOME}|g" packaging/macos/io.github.gigatype.plist \
      >"$LAUNCH_AGENTS/io.github.gigatype.plist"
    launchctl bootout "gui/$(id -u)/io.github.gigatype" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$LAUNCH_AGENTS/io.github.gigatype.plist"
    ;;
  FreeBSD|OpenBSD|NetBSD|DragonFly)
    install -d "$AUTOSTART_DIR"
    install -m 0644 packaging/autostart/io.github.gigatype.autostart.desktop \
      "$AUTOSTART_DIR/io.github.gigatype.autostart.desktop"
    pkill -f 'gigatype daemon' 2>/dev/null || true
    nohup "$BIN_DIR/gigatype" daemon >"$DATA_DIR/daemon.log" 2>&1 &
    ;;
  *)
    echo "Unsupported operating system: $(uname -s)" >&2
    exit 1
    ;;
esac

echo
echo "GigaType 0.3 is installed. Press F9 to start/stop, or Shift+F9 to cancel."
echo "Run 'gigatype doctor' for a health check."
