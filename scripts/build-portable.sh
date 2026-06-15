#!/usr/bin/env bash
# build-portable.sh — produce a zip-based, NSIS-free portable installer for
# Cortex and drop it into the Windows-side Downloads folder.
#
# Usage (from WSL):
#   scripts/build-portable.sh
#
# Output:
#   /mnt/c/Users/<you>/Downloads/cortex-portable-<version>.zip   (if mounted)
#   ~/Downloads/cortex-portable-<version>.zip                     (fallback)
#
# Contents of the zip:
#   cortex.exe       (from `pnpm tauri build ... --no-bundle`)
#   install.bat      (per-user install + Start Menu / optional desktop shortcut)
#   uninstall.bat    (preserves user data)
#   README.txt
#
# This is the fallback to scripts/build-installer.sh when NSIS is not
# available on the build host (NSIS requires `apt install nsis`).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

[[ -d "$HOME/.cargo" ]] && source "$HOME/.cargo/env"

VERSION="$(grep -m1 '"version"' src-tauri/tauri.conf.json | sed -E 's/.*"version"\s*:\s*"([^"]+)".*/\1/')"
EXE_SRC="$ROOT/src-tauri/target/x86_64-pc-windows-msvc/release/cortex.exe"
PORTABLE_DIR="$ROOT/scripts/portable"

# Pick destination: Windows Downloads if mounted (username from $WINUSER or
# $USER), else WSL ~/Downloads.
WIN_DOWNLOADS="/mnt/c/Users/${WINUSER:-$USER}/Downloads"
if [[ -d "$WIN_DOWNLOADS" ]]; then
  DEST_DIR="$WIN_DOWNLOADS"
else
  DEST_DIR="$HOME/Downloads"
fi
mkdir -p "$DEST_DIR"
DEST_ZIP="$DEST_DIR/cortex-portable-${VERSION}.zip"

echo "==> Building Cortex portable installer v${VERSION}"

# Reuse last build if cortex.exe already exists and is newer than this script;
# otherwise rebuild with --no-bundle (no NSIS needed).
if [[ ! -f "$EXE_SRC" ]]; then
  echo "==> No prior build at $EXE_SRC — running pnpm tauri build --no-bundle"
  pnpm tauri build --target x86_64-pc-windows-msvc --runner cargo-xwin --no-bundle
fi

if [[ ! -f "$EXE_SRC" ]]; then
  echo "ERROR: cortex.exe still missing at $EXE_SRC after build." >&2
  exit 1
fi

# Stage files into a temp dir so the zip has a clean top-level layout.
STAGE="$(mktemp -d -t cortex-portable.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT

cp "$EXE_SRC"                 "$STAGE/cortex.exe"
cp "$PORTABLE_DIR/install.bat"    "$STAGE/install.bat"
cp "$PORTABLE_DIR/uninstall.bat"  "$STAGE/uninstall.bat"
cp "$PORTABLE_DIR/README.txt"     "$STAGE/README.txt"

rm -f "$DEST_ZIP"

# Use `zip` if available; otherwise fall back to python3's zipfile module so
# this works on hosts without the `zip` apt package installed.
if command -v zip >/dev/null 2>&1; then
  ( cd "$STAGE" && zip -q -9 "$DEST_ZIP" cortex.exe install.bat uninstall.bat README.txt )
elif command -v python3 >/dev/null 2>&1; then
  echo "    (note: 'zip' not installed — using python3 -m zipfile fallback)"
  ( cd "$STAGE" && python3 -m zipfile -c "$DEST_ZIP" cortex.exe install.bat uninstall.bat README.txt )
else
  echo "ERROR: neither 'zip' nor 'python3' available to create the archive." >&2
  echo "       Install one:  sudo apt install -y zip" >&2
  exit 1
fi

echo
echo "==> Portable installer ready:"
echo "    Path:    $DEST_ZIP"
if [[ "$DEST_ZIP" == /mnt/c/* ]]; then
  WIN_PATH="$(echo "$DEST_ZIP" | sed -E 's|^/mnt/c|C:|; s|/|\\|g')"
  echo "    Windows: $WIN_PATH"
fi
echo "    Size:    $(du -h "$DEST_ZIP" | cut -f1)"
echo
echo "User flow:"
echo "  1. Download/extract the zip on Windows"
echo "  2. Double-click install.bat"
echo "  3. Cortex installs to %LOCALAPPDATA%\\Cortex and launches"
