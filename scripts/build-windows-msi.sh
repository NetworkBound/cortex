#!/usr/bin/env bash
# Cross-compile the Cortex Windows NSIS installer from Linux / WSL using
# cargo-xwin. This is Tauri's *experimental* Linux→Windows path: it produces the
# per-user NSIS `-setup.exe`. The WiX `.msi` can only be built on Windows
# (WiX needs the Windows toolset) — for an MSI use `pnpm tauri build` on a
# Windows box or the `build-msi.yml` workflow. The script keeps its historical
# name so existing references still work.
#
# Prereqs (install via sudo before running this):
#   sudo apt install -y build-essential clang lld llvm nsis pkg-config \
#                       libssl-dev libdbus-1-dev curl wget
# Plus Go 1.26+ (for the tsnet sidecar) and the Rust target (added below).
#
# Output: src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*-setup.exe
#
# The result is UNSIGNED: tauri.conf.json's certificateThumbprint refers to a
# cert in a Windows certificate store, which signtool can't reach from Linux.
# Sign afterwards on Windows with scripts/sign-windows.ps1 (or build on Windows
# so the inner cortex.exe gets signed too — see docs/WINDOWS-BUILD.md).

set -euo pipefail
cd "$(dirname "$0")/.."

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

TARGET=x86_64-pc-windows-msvc

echo "==> Verifying prereqs"
for bin in clang rustup cargo pnpm go makensis; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    case "$bin" in
      makensis) echo "missing: makensis (apt: nsis). NSIS is required to build the -setup.exe." ;;
      go)       echo "missing: go. The tsnet sidecar is a Go program (Go 1.26+)." ;;
      *)        echo "missing: $bin. Run the sudo apt install at the top of this script first." ;;
    esac
    exit 1
  fi
done
# On Debian/Ubuntu lld ships as `lld` and exposes `lld-link` as an alternative.
if ! command -v lld-link >/dev/null 2>&1 && ! command -v lld >/dev/null 2>&1; then
  echo "missing: lld-link (apt: lld). Run the sudo apt install at the top of this script first."
  exit 1
fi

echo "==> Adding $TARGET target"
rustup target add "$TARGET"

echo "==> Installing cargo-xwin (if missing)"
if ! command -v cargo-xwin >/dev/null 2>&1; then
  cargo install --locked cargo-xwin
fi

echo "==> Installing JS deps"
pnpm install --frozen-lockfile

echo "==> Generating icons (if source.png exists)"
if [ -f src-tauri/icons/source.png ] && [ ! -f src-tauri/icons/icon.ico ]; then
  pnpm tauri icon src-tauri/icons/source.png
fi

# tauri-build resolves bundle.externalBin at compile time, so the sidecar must
# exist as src-tauri/binaries/cortex-tsnet-$TARGET.exe before cargo runs.
echo "==> Building tsnet sidecar for $TARGET"
bash scripts/build-tsnet-sidecar.sh "$TARGET"

echo "==> Cross-compiling Tauri to $TARGET (NSIS installer)"
# `tauri build` runs `pnpm build` itself (beforeBuildCommand). tauri-cli
# respects --runner so cargo-xwin handles linking + the xwin SDK download.
pnpm tauri build --target "$TARGET" --runner cargo-xwin --bundles nsis

echo
echo "==> Done. Artifacts:"
find "src-tauri/target/$TARGET/release/bundle" -type f -name "*.exe" | sed 's/^/  /'
