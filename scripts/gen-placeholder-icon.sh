#!/usr/bin/env bash
# Generate a placeholder 1024x1024 PNG and run `tauri icon` to produce all
# the bundle icon sizes. Real branded icon should replace `source.png` later.
set -euo pipefail

cd "$(dirname "$0")/.."

# ImageMagick 7 ships `magick`; 6 ships `convert`. Accept either.
if command -v magick >/dev/null; then
  IM=magick
elif command -v convert >/dev/null; then
  IM=convert
else
  echo "ImageMagick not found. Install: sudo apt install -y imagemagick" >&2
  exit 1
fi

SRC=src-tauri/icons/source.png
"$IM" -size 1024x1024 \
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
