#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
APPDIR=${ROOT}/target/appimage/GigaType.AppDir
rm -rf "$APPDIR"
install -d "$APPDIR/usr/bin"
install -m 0755 "$ROOT/target/release/gigatype" "$APPDIR/usr/bin/gigatype"
install -m 0755 "$ROOT/packaging/appimage/AppRun" "$APPDIR/AppRun"
install -m 0644 "$ROOT/packaging/appimage/io.github.gigatype.desktop" "$APPDIR/io.github.gigatype.desktop"
appimagetool "$APPDIR" "$ROOT/target/GigaType-x86_64.AppImage"
