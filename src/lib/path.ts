/**
 * Path helpers that work for both OS families the desktop app targets.
 *
 * The backend hands the frontend whatever the host OS produced — `/` on
 * Linux/macOS, `\` (or a `/`-`\` mix, e.g. a Windows root joined with a
 * forward-slash relative id) on Windows — so every helper here treats both
 * separators as equivalent. Display helpers preserve the separators of their
 * input; joins use `/`, which the Rust `fs` layer accepts on Windows too.
 *
 * Kept dependency-free on purpose: `node:path` isn't available in the
 * WebView, and `@tauri-apps/api/path` is async.
 */

/** Matches a single path separator of either family. */
const SEP = /[\\/]/;

/** `true` when `p` ends with a separator (`/`, `\`). */
function endsWithSep(p: string): boolean {
  return /[\\/]$/.test(p);
}

/**
 * Whether `p` is absolute on either OS: `/x`, `\x`, `C:\x`, `C:/x`, or a UNC
 * share (`\\server\share`, which starts with `\` and so matches too).
 */
export function isAbsolute(p: string): boolean {
  return /^(?:[\\/]|[A-Za-z]:[\\/])/.test(p);
}

/** Replace every `\` with `/` (for display or for backend paths we build). */
export function toPosix(p: string): string {
  return p.replace(/\\/g, "/");
}

/**
 * Strip trailing separators. A bare root (`/`, `\`, `C:\`, `C:/`) is
 * returned unchanged so it doesn't collapse to `""` or `C:`.
 */
export function stripTrailingSep(p: string): string {
  const stripped = p.replace(/[\\/]+$/, "");
  if (stripped === "" || /^[A-Za-z]:$/.test(stripped)) {
    return p.length > stripped.length ? stripped + p[stripped.length] : p;
  }
  return stripped;
}

/**
 * Last non-empty path segment (`/a/b/c.txt` → `c.txt`, `C:\a\b\` → `b`).
 * Returns `p` unchanged when there is no such segment (`""`, `/`).
 */
export function basename(p: string): string {
  const m = /([^\\/]+)[\\/]*$/.exec(p);
  return m ? m[1] : p;
}

/**
 * Everything before the final separator, with the input's separators kept
 * (`src/lib/x.ts` → `src/lib`, `C:\a\b.rs` → `C:\a`). `""` when `p` has no
 * separator; the root itself (`/`) when the only separator is the first char.
 */
export function dirname(p: string): string {
  const trimmed = stripTrailingSep(p);
  if (trimmed !== p && trimmed.length <= 1) return trimmed; // bare root
  const m = /[\\/](?=[^\\/]*$)/.exec(trimmed);
  if (!m) return "";
  return m.index === 0 ? trimmed.slice(0, 1) : trimmed.slice(0, m.index);
}

/**
 * Join segments with `/`. Trailing separators are stripped from every part
 * and leading ones from every part after the first, so
 * `join("C:\\proj\\", "/src/a.rs")` → `C:\proj/src/a.rs`. Empty parts are
 * skipped. The first part's own separators are preserved (the backend
 * accepts mixed separators on Windows).
 */
export function join(...parts: string[]): string {
  const [head, ...rest] = parts;
  const tail = rest
    .map((p) => p.replace(/^[\\/]+/, "").replace(/[\\/]+$/, ""))
    .filter((p) => p.length > 0);
  if (head === undefined || head === "") return tail.join("/");
  if (tail.length === 0) return head;
  const first = stripTrailingSep(head);
  return endsWithSep(first)
    ? first + tail.join("/")
    : `${first}/${tail.join("/")}`;
}

/**
 * `path` relative to `root`, or `null` when `path` is not inside `root`.
 * Comparison ignores separator flavour and a trailing separator on `root`,
 * and requires a segment boundary (`/p/proj` does not contain `/p/proj2/x`).
 * Returns `""` when `path` *is* the root, otherwise the remaining segment(s)
 * of `path` with their original separators and no leading separator.
 */
export function relativeTo(
  path: string,
  root: string | null | undefined,
): string | null {
  if (!root) return null;
  const r = stripTrailingSep(root);
  const normPath = toPosix(path);
  const normRoot = toPosix(r);
  if (!normPath.startsWith(normRoot)) return null;
  const restStart = normRoot.length;
  if (normPath.length === restStart) return "";
  // Boundary: the root itself ends in a separator (`/`, `C:/`) or the next
  // char of `path` is one.
  if (!endsWithSep(normRoot) && !SEP.test(normPath[restStart])) return null;
  return path.slice(restStart).replace(/^[\\/]+/, "");
}

/**
 * Compact form of `path` for headers and list rows: project-relative when it
 * lives under `root`, otherwise `…/` + the last `keep` segments (or the path
 * itself when it has no more than `keep` segments).
 */
export function shortenPath(
  path: string,
  root?: string | null,
  keep = 3,
): string {
  const rel = relativeTo(path, root);
  if (rel !== null) return rel;
  const parts = path.split(SEP).filter(Boolean);
  if (parts.length <= keep) return path;
  return "…/" + parts.slice(-keep).join("/");
}
