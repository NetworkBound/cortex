#!/bin/sh
# Build the tsnet Go sidecar for a given Rust target triple, naming the output
# binary the way Tauri's externalBin bundling expects: cortex-tsnet-<triple>[.exe].
#
# Usage:
#   scripts/build-tsnet-sidecar.sh [rust-target-triple]
#
# If no triple is passed, the host triple is derived from `rustc -vV`.
# Works under POSIX sh on Linux, macOS, and Windows git-bash.
set -eu

# Resolve repo root relative to this script (scripts/ is a direct child of root).
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

# 1. Determine the Rust target triple.
TRIPLE="${1:-}"
if [ -z "$TRIPLE" ]; then
  TRIPLE=$(rustc -vV | grep '^host:' | sed 's/^host: //')
fi
if [ -z "$TRIPLE" ]; then
  echo "error: could not determine Rust target triple" >&2
  exit 1
fi

# 2. Map the triple -> GOOS / GOARCH (+ exe suffix).
EXT=""
case "$TRIPLE" in
  x86_64-pc-windows-msvc)    GOOS=windows; GOARCH=amd64; EXT=".exe" ;;
  aarch64-pc-windows-msvc)   GOOS=windows; GOARCH=arm64; EXT=".exe" ;;
  x86_64-apple-darwin)       GOOS=darwin;  GOARCH=amd64 ;;
  aarch64-apple-darwin)      GOOS=darwin;  GOARCH=arm64 ;;
  x86_64-unknown-linux-gnu)  GOOS=linux;   GOARCH=amd64 ;;
  aarch64-unknown-linux-gnu) GOOS=linux;   GOARCH=arm64 ;;
  *)
    echo "error: unsupported target triple: $TRIPLE" >&2
    exit 1
    ;;
esac

# 3. Build into src-tauri/binaries/cortex-tsnet-<triple>[.exe].
OUT_DIR="$REPO_ROOT/src-tauri/binaries"
mkdir -p "$OUT_DIR"
OUT_PATH="$OUT_DIR/cortex-tsnet-$TRIPLE$EXT"

(
  cd "$REPO_ROOT/sidecar/cortex-tsnet"
  GOOS="$GOOS" GOARCH="$GOARCH" CGO_ENABLED=0 go build -o "$OUT_PATH" .
)

echo "$OUT_PATH"
