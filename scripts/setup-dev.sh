#!/usr/bin/env bash
set -euo pipefail

# Hermes dev setup — installs the system deps Tauri needs on Linux and
# verifies the toolchain is present. Idempotent.

echo "==> Checking toolchain"
command -v cargo >/dev/null || {
  echo "Rust not found. Install with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
}
command -v node >/dev/null || { echo "Node not found"; exit 1; }
command -v pnpm >/dev/null || { echo "pnpm not found. npm i -g pnpm@9"; exit 1; }

case "$(uname -s)" in
  Linux*)
    echo "==> Installing Linux build deps (sudo required)"
    if command -v apt-get >/dev/null; then
      sudo apt-get update
      sudo apt-get install -y \
        libwebkit2gtk-4.1-dev \
        libssl-dev \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        build-essential \
        curl wget file \
        libxdo-dev \
        libdbus-1-dev \
        pkg-config
    elif command -v dnf >/dev/null; then
      sudo dnf install -y \
        webkit2gtk4.1-devel \
        openssl-devel \
        libappindicator-gtk3-devel \
        librsvg2-devel \
        @development-tools \
        curl wget file \
        libxdo-devel \
        dbus-devel \
        pkgconf-pkg-config
    elif command -v pacman >/dev/null; then
      sudo pacman -Syu --needed --noconfirm \
        webkit2gtk-4.1 \
        openssl \
        libappindicator-gtk3 \
        librsvg \
        base-devel \
        curl wget file \
        xdotool \
        dbus \
        pkgconf
    elif command -v zypper >/dev/null; then
      sudo zypper install -y \
        webkit2gtk3-soup2-devel \
        libopenssl-devel \
        libappindicator3-devel \
        librsvg-devel \
        -t pattern devel_basis \
        curl wget file \
        xdotool-devel \
        dbus-1-devel \
        pkg-config
    else
      echo "Unsupported Linux distro: no apt-get/dnf/pacman/zypper found." >&2
      echo "Install the Tauri prerequisites manually: https://tauri.app/start/prerequisites/" >&2
      exit 1
    fi
    ;;
  Darwin*)
    echo "==> macOS detected — Xcode CLI tools required"
    xcode-select -p >/dev/null || xcode-select --install
    ;;
  MINGW*|MSYS*|CYGWIN*)
    echo "==> Windows detected — install WebView2 runtime if missing"
    ;;
esac

echo "==> Installing JS deps"
pnpm install

echo "==> Done. Run: pnpm tauri:dev"
