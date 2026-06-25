#!/usr/bin/env bash
# Build Hermes for Linux (.deb + .AppImage). Run AFTER the sudo apt install
# in the README.
set -euo pipefail
cd "$(dirname "$0")/.."

source "$HOME/.cargo/env"

echo "==> Installing JS deps"
pnpm install --frozen-lockfile

echo "==> Generating icons (if needed)"
if [ -f src-tauri/icons/source.png ] && [ ! -f src-tauri/icons/icon.png ]; then
  pnpm tauri icon src-tauri/icons/source.png
fi

echo "==> Building frontend"
pnpm build

echo "==> Building Tauri bundles"
# NO_STRIP: linuxdeploy's bundled binutils strip chokes on Fedora 44 system
# libs (`.relr.dyn` / SHT_RELR sections → "Unable to recognise the format"),
# failing the whole AppImage bundle. Skipping strip just ships slightly
# larger libs.
NO_STRIP=true pnpm tauri build --bundles deb,appimage

# The Tauri-built AppImage bundles this build host's webkit2gtk/GTK, which fails
# EGL init (black screen) on targets with much newer Mesa (e.g. Fedora 44).
# Rewrite it to prefer the host's WebKit, keeping bundled libs as a fallback.
echo "==> Patching AppImage to use host WebKit (avoids black screen on modern Mesa)"
bash scripts/fix-appimage-host-libs.sh

echo
echo "==> Done. Artifacts:"
find src-tauri/target/release/bundle -type f \( -name "*.deb" -o -name "*.AppImage" \) | sed 's/^/  /'
