#!/usr/bin/env bash
# Build Cortex for Linux (.deb + .AppImage). Run AFTER `scripts/setup-dev.sh`
# (or the apt install in the README) so the webkit2gtk/GTK dev packages exist.
# Also needs Go 1.26+ for the tsnet sidecar and squashfs-tools (mksquashfs)
# for the AppImage post-processing step.
set -euo pipefail
cd "$(dirname "$0")/.."

# rustup installs put cargo on PATH via this file; a distro-packaged cargo
# doesn't have it, so only source it when present.
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

echo "==> Installing JS deps"
pnpm install --frozen-lockfile

echo "==> Generating icons (if needed)"
if [ -f src-tauri/icons/source.png ] && [ ! -f src-tauri/icons/icon.png ]; then
  pnpm tauri icon src-tauri/icons/source.png
fi

# tauri-build resolves bundle.externalBin at compile time, so the sidecar must
# exist as src-tauri/binaries/cortex-tsnet-<host-triple> before cargo runs.
echo "==> Building tsnet sidecar"
bash scripts/build-tsnet-sidecar.sh

echo "==> Building Tauri bundles"
# `tauri build` runs `pnpm build` itself (beforeBuildCommand), so the frontend
# is not built separately here.
# NO_STRIP: linuxdeploy's bundled binutils strip chokes on Fedora 44 system
# libs (`.relr.dyn` / SHT_RELR sections → "Unable to recognise the format"),
# failing the whole AppImage bundle. Skipping strip just ships slightly
# larger libs.
# --remap-path-prefix keeps $HOME out of panic messages and debug info.
RUSTFLAGS="--remap-path-prefix=$HOME=" NO_STRIP=true \
  pnpm tauri build --bundles deb,appimage

# The Tauri-built AppImage bundles this build host's webkit2gtk/GTK, which fails
# EGL init (black screen) on targets with much newer Mesa (e.g. Fedora 44).
# Rewrite it to prefer the host's WebKit, keeping bundled libs as a fallback.
echo "==> Patching AppImage to use host WebKit (avoids black screen on modern Mesa)"
bash scripts/fix-appimage-host-libs.sh

echo
echo "==> Done. Artifacts:"
find src-tauri/target/release/bundle -type f \( -name "*.deb" -o -name "*.AppImage" \) | sed 's/^/  /'
