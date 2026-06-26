#!/usr/bin/env bash
# Cross-compile Hermes to a Windows .msi from WSL Linux using cargo-xwin.
# Prereqs (must be installed via sudo before running this):
#   sudo apt install -y build-essential clang lld llvm pkg-config libssl-dev \
#                       libdbus-1-dev curl wget
# Plus the rust target (the script will add it if missing).
#
# Output: src-tauri/target/x86_64-pc-windows-msvc/release/bundle/{msi,nsis}/*.{msi,exe}

set -euo pipefail
cd "$(dirname "$0")/.."

source "$HOME/.cargo/env"

echo "==> Verifying prereqs"
for bin in clang lld-link rustup cargo pnpm; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    if [ "$bin" = "lld-link" ]; then
      # On Debian/Ubuntu lld ships as `lld` and exposes `lld-link` as an alternative;
      # see /usr/bin/lld-link or use `lld -flavor link`.
      if ! command -v lld >/dev/null 2>&1; then
        echo "missing: $bin (apt: lld). Run the sudo apt install in the README first."
        exit 1
      fi
    else
      echo "missing: $bin. Run the sudo apt install in the README first."
      exit 1
    fi
  fi
done

echo "==> Adding x86_64-pc-windows-msvc target"
rustup target add x86_64-pc-windows-msvc

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

echo "==> Building frontend (Vite)"
pnpm build

echo "==> Cross-compiling Tauri to x86_64-pc-windows-msvc"
# tauri-cli respects --runner so cargo-xwin handles linking + xwin SDK download.
pnpm tauri build --target x86_64-pc-windows-msvc --runner cargo-xwin --bundles msi,nsis

echo
echo "==> Done. Artifacts:"
find src-tauri/target/x86_64-pc-windows-msvc/release/bundle -type f \( -name "*.msi" -o -name "*.exe" \) | sed 's/^/  /'
