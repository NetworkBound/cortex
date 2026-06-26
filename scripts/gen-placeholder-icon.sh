#!/usr/bin/env bash
# Generate a placeholder 1024x1024 PNG and run `tauri icon` to produce all
# the bundle icon sizes. Real branded icon should replace `source.png` later.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v convert >/dev/null; then
  echo "ImageMagick 'convert' not found. Install: sudo apt install -y imagemagick"
  exit 1
fi

SRC=src-tauri/icons/source.png
convert -size 1024x1024 \
  -define gradient:angle=135 \
  gradient:'#1f2740-#7c93ff' \
  -gravity Center \
  -fill white \
  -font 'DejaVu-Sans-Bold' \
  -pointsize 720 \
  -annotate +0+0 'H' \
  "$SRC"
echo "wrote $SRC"
pnpm tauri icon "$SRC"
