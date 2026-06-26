import { invoke } from "@tauri-apps/api/core";

/**
 * Structured result of a `git pull --ff-only` from the backend.
 * Mirrors the Rust `PullResult` struct one-for-one.
 */
export interface PullResult {
  /** `true` iff git exited 0. */
  ok: boolean;
  /** Tail of stdout (last 4 KiB). */
  stdout_tail: string;
  /** Tail of stderr (last 4 KiB). */
  stderr_tail: string;
  /** Process exit code, or -1 when git was killed before returning one. */
  exit_code: number;
}

/**
 * `git pull --ff-only`. Prefers a fast-forward to avoid surprise merge
 * commits; a non-fast-forward situation comes back as `result.ok === false`
 * with the rejection in `stderr_tail`. Returns a structured result; never
 * throws on a non-zero git exit (callers inspect `result.ok`).
 *
 * Throws only when the spawn itself fails (git missing, project root not a
 * directory, etc.).
 */
export async function gitPull(projectRoot: string): Promise<PullResult> {
  return invoke<PullResult>("git_pull", { projectRoot });
}

/**
 * Compact one-line render of a [`PullResult`] for inline display. Returns
 * stderr when present (pull reports rejections + hints there), falling back to
 * stdout, then a generic exit-code string.
 */
export function summarizePullResult(r: PullResult): string {
  const last = (s: string) => {
    const lines = s.split(/\r?\n/).filter((l) => l.trim());
    return lines[lines.length - 1] ?? "";
  };
  const tail = last(r.stderr_tail) || last(r.stdout_tail);
  if (tail) return tail;
  return r.ok ? `exit 0` : `exit ${r.exit_code}`;
}
