/**
 * Background-image persistence + URL helpers.
 *
 * The Rust side (`set_bg_image` command) handles the actual file copy into
 * `~/.cortex/bg/active.<ext>` and persists the path inside the shared
 * `~/.cortex/themes.json` state. This module just exposes a thin async
 * surface to the renderer + converts the absolute filesystem path into a
 * Tauri asset URL the `<img>`/CSS layer can actually load.
 *
 * `convertFileSrc` is the supported way to turn a host path into an
 * `asset://` URL — the CSP in `tauri.conf.json` already allows that scheme
 * on `img-src`.
 */
import { invoke } from "@tauri-apps/api/core";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type { ActiveThemeState } from "./themes-custom";

/** Convert the persisted absolute path into an asset URL the webview can load. */
export function bgImageAssetUrl(path: string | null | undefined): string | null {
  if (!path) return null;
  try {
    return convertFileSrc(path);
  } catch {
    return null;
  }
}

/**
 * Open the native file picker filtered to image extensions, then ask the
 * backend to copy the chosen file into the cortex bg dir. Returns the
 * updated state, or `null` if the user cancelled.
 */
export async function pickAndSetBgImage(): Promise<ActiveThemeState | null> {
  const selected = await openDialog({
    multiple: false,
    directory: false,
    filters: [
      {
        name: "Image",
        // Keep this list aligned with the whitelist in
        // `commands/themes.rs::set_bg_image`. Adding here without adding
        // there will surface a "unsupported extension" error.
        extensions: ["png", "jpg", "jpeg", "webp", "gif", "bmp", "avif"],
      },
    ],
  });
  if (!selected || typeof selected !== "string") return null;
  return invoke<ActiveThemeState>("set_bg_image", { source: selected });
}

/** Clear the persisted background and remove any cached active.* file. */
export async function clearBgImage(): Promise<ActiveThemeState> {
  return invoke<ActiveThemeState>("set_bg_image", { source: null });
}
