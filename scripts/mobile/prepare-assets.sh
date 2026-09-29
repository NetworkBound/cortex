#!/usr/bin/env bash
# Stage the source artwork @capacitor/assets expects (mobile/native/assets/)
# from the Tauri icon set, then generate icons + splash screens into the
# native projects. Run after `cap add`, before `cap sync`.
#
#   bash scripts/mobile/prepare-assets.sh [--android] [--ios]
#
# Non-fatal by design: a failure leaves the Capacitor template icons in place
# and prints a warning, so a broken generator never blocks an APK.
set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
NATIVE_DIR="$REPO_ROOT/mobile/native"
ASSETS="$NATIVE_DIR/assets"
SRC="$REPO_ROOT/src-tauri/icons/source.png"   # 1024x1024
[ -f "$SRC" ] || SRC="$REPO_ROOT/src-tauri/icons/icon.png"   # 512x512 fallback

mkdir -p "$ASSETS"
# `logo.png` (+ optional logo-dark.png) → icons and splash with the logo
# centred on the --splashBackgroundColor.
cp -f "$SRC" "$ASSETS/logo.png"
cp -f "$SRC" "$ASSETS/logo-dark.png"

cd "$NATIVE_DIR"
if npx --no-install capacitor-assets generate "$@" \
    --iconBackgroundColor '#0a0a0b' --iconBackgroundColorDark '#0a0a0b' \
    --splashBackgroundColor '#0a0a0b' --splashBackgroundColorDark '#0a0a0b'; then
  echo "prepare-assets: generated icons/splash for: $*"
else
  echo "::warning::prepare-assets: @capacitor/assets failed; keeping template icons"
fi
