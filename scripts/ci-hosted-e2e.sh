#!/usr/bin/env bash
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true ]]; then
  echo "This destructive virtual-desktop harness is only for disposable GitHub-hosted runners." >&2
  exit 2
fi
if [[ "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
  echo "Refusing to run outside a GitHub-hosted runner." >&2
  exit 2
fi

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
RUNNER_HOME=$HOME
ARTIFACT_DIR=${RUNNER_TEMP}/gigatype-e2e-artifacts
RUNTIME_DIR=${RUNNER_TEMP}/gigatype-runtime
TEST_HOME=${RUNNER_TEMP}/gigatype-home
FIXTURE=${RUNNER_TEMP}/example.wav
EDITOR_OUTPUT=${ARTIFACT_DIR}/editor-output.txt
EXPECTED_OUTPUT=${ARTIFACT_DIR}/expected-transcript.txt
ACTUAL_OUTPUT=${ARTIFACT_DIR}/actual-transcript.txt
CLIPBOARD_SENTINEL='clipboard survives GigaType E2E'
FIXTURE_URL=https://cdn.chatwm.opensmodel.sberdevices.ru/GigaAM/example.wav
FIXTURE_SHA256=d8aaaa18a5098d7c6de0595ae7ac1e64cacd0d4022af3595213bdaf23be77e69

daemon_pid=
editor_pid=
xvfb_pid=

cleanup() {
  local status=$?
  set +e
  [[ -n "$daemon_pid" ]] && kill "$daemon_pid" 2>/dev/null
  [[ -n "$editor_pid" ]] && kill "$editor_pid" 2>/dev/null
  [[ -n "$xvfb_pid" ]] && kill "$xvfb_pid" 2>/dev/null
  pactl list short sinks >"${ARTIFACT_DIR}/pulse-sinks.txt" 2>&1
  pactl list short sources >"${ARTIFACT_DIR}/pulse-sources.txt" 2>&1
  exit "$status"
}
trap cleanup EXIT

mkdir -p "$ARTIFACT_DIR" "$RUNTIME_DIR" "$TEST_HOME"
chmod 0700 "$RUNTIME_DIR"
export DISPLAY=:99
export HOME=$TEST_HOME
export RUSTUP_HOME=${RUSTUP_HOME:-${RUNNER_HOME}/.rustup}
export XDG_RUNTIME_DIR=$RUNTIME_DIR
export GIGATYPE_MODEL=${GIGATYPE_MODEL:-${RUNNER_TEMP}/gigatype-e2e-model}
export GIGATYPE_NO_NOTIFY=1
export GIGATYPE_NO_SOUNDS=1
export GIGATYPE_NO_SESSION_MONITOR=1
export GIGATYPE_KEEP_MEDIA_PLAYING=1
export GIGATYPE_NO_NATIVE_SHORTCUTS=1
export GIGATYPE_NO_PORTAL=1
export GIGATYPE_DEBOUNCE_MS=0
export GIGATYPE_PASTE_KEYS=shift+insert

normalize_words() {
  python3 -c 'import re, sys; print(" ".join(re.findall(r"[^\W_]+", sys.stdin.read().casefold())))'
}

Xvfb "$DISPLAY" -screen 0 1280x720x24 -nolisten tcp >"${ARTIFACT_DIR}/xvfb.log" 2>&1 &
xvfb_pid=$!
for _ in $(seq 1 100); do
  xdpyinfo >/dev/null 2>&1 && break
  sleep 0.1
done
xdpyinfo >/dev/null
autocutsel -fork -selection CLIPBOARD

pulseaudio --start --exit-idle-time=-1 --log-target="file:${ARTIFACT_DIR}/pulseaudio.log"
for _ in $(seq 1 100); do
  pactl info >/dev/null 2>&1 && break
  sleep 0.1
done
pactl info >"${ARTIFACT_DIR}/pulse-info.txt"
pactl load-module module-null-sink \
  sink_name=gigatype_e2e \
  sink_properties=device.description=GigaTypeE2E >"${ARTIFACT_DIR}/pulse-module-id.txt"
pactl set-default-sink gigatype_e2e
pactl set-default-source gigatype_e2e.monitor

curl --fail --location --retry 3 --output "$FIXTURE" "$FIXTURE_URL"
printf '%s  %s\n' "$FIXTURE_SHA256" "$FIXTURE" | sha256sum --check -

"${ROOT}/scripts/bootstrap-model.sh"
cargo build --release --locked
BIN=${ROOT}/target/release/gigatype

expected=$("$BIN" transcribe-file "$FIXTURE")
[[ -n "$expected" ]]
printf '%s\n' "$expected" >"$EXPECTED_OUTPUT"

printf '%s' "$CLIPBOARD_SENTINEL" | xclip -selection clipboard -in
[[ "$(xclip -selection clipboard -out)" == "$CLIPBOARD_SENTINEL" ]]

python3 "${ROOT}/scripts/ci-x11-editor.py" "$EDITOR_OUTPUT" \
  >"${ARTIFACT_DIR}/editor.log" 2>&1 &
editor_pid=$!
window_id=$(timeout 15 xdotool search --sync --onlyvisible --name GigaType-E2E | head -n 1)
xdotool windowfocus --sync "$window_id"
xdotool getwindowfocus >"${ARTIFACT_DIR}/focused-window.txt"

$BIN daemon >"${ARTIFACT_DIR}/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 1 600); do
  if [[ -S "${RUNTIME_DIR}/gigatype.sock" ]] && timeout 1 "$BIN" status 2>/dev/null | grep -qx idle; then
    break
  fi
  kill -0 "$daemon_pid"
  sleep 0.1
done
timeout 2 "$BIN" status | grep -qx idle

"$BIN" toggle | grep -qx recording
paplay --device=gigatype_e2e "$FIXTURE"
actual=$("$BIN" toggle)
[[ -n "$actual" ]]
printf '%s\n' "$actual" >"$ACTUAL_OUTPUT"

for _ in $(seq 1 200); do
  [[ -f "$EDITOR_OUTPUT" ]] && break
  kill -0 "$editor_pid" 2>/dev/null || true
  sleep 0.1
done
if [[ ! -f "$EDITOR_OUTPUT" ]]; then
  xclip -selection clipboard -out >"${ARTIFACT_DIR}/clipboard-after-failure.txt" 2>&1 || true
  echo "The focused X11 editor received no paste event." >&2
  exit 1
fi
editor_text=$(<"$EDITOR_OUTPUT")
restored_clipboard=$(timeout 5 xclip -selection clipboard -out)

expected_words=$(printf '%s' "$expected" | normalize_words)
actual_words=$(printf '%s' "$actual" | normalize_words)
if [[ "$actual_words" != "$expected_words" ]]; then
  echo "Virtual microphone transcription differs word-for-word from direct-file transcription." >&2
  diff -u "$EXPECTED_OUTPUT" "$ACTUAL_OUTPUT" >&2 || true
  exit 1
fi
if [[ "$editor_text" != "$actual" ]]; then
  echo "The focused X11 editor did not receive the complete transcription." >&2
  diff -u "$ACTUAL_OUTPUT" "$EDITOR_OUTPUT" >&2 || true
  exit 1
fi
if [[ "$restored_clipboard" != "$CLIPBOARD_SENTINEL" ]]; then
  echo "GigaType did not restore the original X11 clipboard." >&2
  exit 1
fi
grep -q 'GigaType microphone:' "${ARTIFACT_DIR}/daemon.log"

echo "Hosted E2E passed: virtual microphone == direct GigaAM == focused X11 editor"
