#!/usr/bin/env bash
#
# fix-appimage-host-libs.sh — post-process the Tauri-built AppImage so it
# RENDERS on modern distros (Fedora, Arch, …) instead of showing a black window.
#
# WHY: `tauri build` bundles its build host's libwebkit2gtk-4.1 + GTK stack into
# the AppImage. On a target with a much newer Mesa (e.g. Fedora 44 / Mesa 26),
# that older bundled WebKit cannot initialise EGL — the web process aborts with
# "Could not create default EGL display: EGL_BAD_PARAMETER" and the whole UI is
# a black rectangle. No runtime env var (WEBKIT_DISABLE_DMABUF_RENDERER,
# GDK_BACKEND=x11, LIBGL_ALWAYS_SOFTWARE, sandbox toggles) fixes it because the
# incompatible library is *inside* the bundle.
#
# FIX: move the bundled GTK/GLib/WebKit graph aside and install a smart AppRun
# that PREFERS the host's WebKit (so it renders on a host that has
# webkit2gtk-4.1 — exactly what the .deb already declares as a dependency), and
# only falls back to the bundled libs when the host lacks WebKit. The whole
# graph must move together: mixing host GTK with bundled GLib triggers symbol
# errors (host libsecret needs newer glib symbols than the bundled glib has).
#
# Repacks WITHOUT appimagetool: a type-2 AppImage is just the original runtime
# header followed by a squashfs image, so we reuse the header and rebuild the
# squashfs with mksquashfs.
#
# Idempotent-ish: operates on the single .AppImage under the bundle dir.
set -euo pipefail

cd "$(dirname "$0")/.."
BUNDLE_DIR="src-tauri/target/release/bundle/appimage"

APPIMAGE="$(find "$BUNDLE_DIR" -maxdepth 1 -name '*.AppImage' | head -1)"
if [ -z "${APPIMAGE:-}" ]; then
  echo "fix-appimage: no .AppImage found under $BUNDLE_DIR — nothing to do" >&2
  exit 0
fi
echo "==> fix-appimage: $APPIMAGE ($(du -h "$APPIMAGE" | cut -f1))"

command -v mksquashfs >/dev/null || { echo "fix-appimage: mksquashfs not found (install squashfs-tools)" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# 1) Capture the runtime header (everything before the squashfs image).
OFFSET="$("$APPIMAGE" --appimage-offset)"
head -c "$OFFSET" "$APPIMAGE" > "$WORK/runtime"
echo "==> runtime header: $OFFSET bytes"

# 2) Extract the AppImage payload.
( cd "$WORK" && "$OLDPWD/$APPIMAGE" --appimage-extract >/dev/null )
ROOT="$WORK/squashfs-root"

# 3) Move the bundled GTK/GLib/WebKit graph aside, keep it as a fallback.
#    We relocate the ENTIRE usr/lib (the graph is densely interdependent and
#    partial removal breaks symbol resolution). The binary's RUNPATH is
#    $ORIGIN/../lib, so an empty usr/lib makes it resolve against host libs.
mv "$ROOT/usr/lib" "$ROOT/usr/lib.bundled"
mkdir -p "$ROOT/usr/lib"

# 4) Install a smart AppRun: prefer host WebKit, fall back to bundled.
cat > "$ROOT/AppRun" <<'APPRUN'
#!/bin/sh
# Smart launcher: prefer the host WebKit/GTK stack (renders on modern Mesa),
# fall back to the bundled libs when the host has no webkit2gtk-4.1.
HERE="$(dirname "$(readlink -f "$0")")"
export APPDIR="$HERE"

have_host_webkit() {
  if command -v ldconfig >/dev/null 2>&1; then
    ldconfig -p 2>/dev/null | grep -q 'libwebkit2gtk-4.1\.so\.0' && return 0
  fi
  for d in /usr/lib64 /usr/lib/x86_64-linux-gnu /usr/lib /lib64; do
    [ -e "$d/libwebkit2gtk-4.1.so.0" ] && return 0
  done
  return 1
}

if have_host_webkit; then
  # usr/lib is empty -> binary RUNPATH ($ORIGIN/../lib) resolves to host libs.
  unset LD_LIBRARY_PATH
else
  # No host WebKit: use the libs we shipped.
  export LD_LIBRARY_PATH="$HERE/usr/lib.bundled${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export GIO_MODULE_DIR="$HERE/usr/lib.bundled/gio/modules"
fi

exec "$HERE/usr/bin/cortex" "$@"
APPRUN
chmod +x "$ROOT/AppRun"

# 5) Repack: runtime header + fresh squashfs.
mksquashfs "$ROOT" "$WORK/payload.sqfs" -comp gzip -noappend -root-owned -no-progress >/dev/null
cat "$WORK/runtime" "$WORK/payload.sqfs" > "$APPIMAGE.fixed"
chmod +x "$APPIMAGE.fixed"
mv "$APPIMAGE.fixed" "$APPIMAGE"

echo "==> fix-appimage: done — $(du -h "$APPIMAGE" | cut -f1) (host-WebKit preferred, bundled fallback retained)"
