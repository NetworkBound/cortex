/**
 * "Open in…" bridge — hand a project, folder or file to the user's editor,
 * file manager or a terminal window.
 *
 * Wraps the Rust commands in `src-tauri/src/commands/open_external.rs`:
 *   - `open_in_editor(path, line?, editor?)` → program launched
 *   - `reveal_in_file_manager(path)`
 *   - `open_terminal_here(dir)` → program launched
 *
 * The backend confines every path to a known project / config root and
 * spawns the child detached with the target as a separate argv entry, so
 * nothing here has to quote or escape. Paths are passed through verbatim —
 * on Windows they keep their backslashes (the backend canonicalizes).
 *
 * Feature detection: a build whose backend predates these commands rejects
 * the invoke with Tauri's "Command … not found". `isUnavailable()` recognises
 * that shape so callers can show "not available in this build" instead of a
 * raw IPC error, and `describeOpenError()` turns any failure into one toast-
 * ready sentence.
 */

import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";

/** Raw text of an invoke rejection (Tauri hands back a string or an Error). */
function rawText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  try {
    return JSON.stringify(e) ?? String(e);
  } catch {
    return String(e);
  }
}

/** True when the rejection means the backend has no such command — i.e. the
 *  running build was compiled without `commands/open_external.rs`. */
export function isUnavailable(e: unknown): boolean {
  const s = rawText(e);
  return /command\s+\S+\s+not\s+found|not\s+allowed\s+by\s+.*capabilit/i.test(
    s,
  );
}

/** One-sentence explanation of a failed open, for a toast body. */
export function describeOpenError(e: unknown): string {
  if (isUnavailable(e)) return "Not available in this build of Cortex.";
  return humanizeError(e);
}

/** Open `path` (optionally at `line`) in the user's editor. Resolution order
 *  lives in the backend: explicit `editor` → `CORTEX_EDITOR` / `$VISUAL` /
 *  `$EDITOR` → well-known GUI editors → OS default handler. Resolves to the
 *  program that was launched. */
export async function openInEditor(
  path: string,
  line?: number,
  editor?: string,
): Promise<string> {
  return invoke<string>("open_in_editor", {
    path,
    line: line ?? null,
    editor: editor ?? null,
  });
}

/** Reveal `path` in Explorer / Finder / the default Linux file manager. */
export async function revealInFileManager(path: string): Promise<void> {
  return invoke<void>("reveal_in_file_manager", { path });
}

/** Open a terminal window with `dir` as its working directory (a file path
 *  opens the terminal in the file's folder). Resolves to the program
 *  launched. */
export async function openTerminalHere(dir: string): Promise<string> {
  return invoke<string>("open_terminal_here", { dir });
}

/** Copy a filesystem path verbatim (separators untouched) to the clipboard.
 *  Falls back to a hidden textarea + `execCommand` for webviews that gate
 *  the async clipboard API behind focus/permission. */
export async function copyPath(path: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(path);
    return;
  } catch {
    /* fall through to the legacy path */
  }
  const ta = document.createElement("textarea");
  ta.value = path;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try {
    if (!document.execCommand("copy")) throw new Error("copy rejected");
  } finally {
    document.body.removeChild(ta);
  }
}

/** Windows-shaped path: drive letter or UNC prefix. Decides the `cd` quoting
 *  below without a platform API — the path itself tells us the shell family. */
function looksWindows(p: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(p) || p.startsWith("\\\\");
}

/**
 * Build the one-line `cd` the embedded TerminalPane types into a fresh shell so it starts in
 * the project. Backslashes are kept on Windows.
 *
 * - Windows: `cd "<path>"` works in PowerShell 7 / 5.1 (the backend's default)
 *   and cmd.exe alike; the `\\?\` verbatim prefix is stripped because neither
 *   shell accepts it.
 * - POSIX: `cd -- '<path>'` with embedded single quotes escaped, so spaces,
 *   `$`, backticks and `&` are all literal. The leading space keeps the line
 *   out of bash/zsh history under HISTCONTROL=ignorespace.
 */
export function shellCdLine(root: string): string {
  if (looksWindows(root)) {
    const p = root.startsWith("\\\\?\\") ? root.slice(4) : root;
    return ` cd "${p.replace(/"/g, "")}"\r`;
  }
  return ` cd -- '${root.replace(/'/g, "'\\''")}'\r`;
}
