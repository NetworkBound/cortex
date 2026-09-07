import { invoke } from "@tauri-apps/api/core";

/**
 * Structured result of a `git push origin <branch>` from the backend.
 * Mirrors the Rust `PushResult` struct one-for-one.
 */
export interface PushResult {
  /** `true` iff git exited 0. */
  ok: boolean;
  /** Tail of stdout (last 4 KiB). */
  stdout_tail: string;
  /** Tail of stderr (last 4 KiB). */
  stderr_tail: string;
  /** Process exit code, or -1 when git was killed before returning one. */
  exit_code: number;
  /** Branch we actually pushed (`HEAD` when none was supplied). */
  branch: string;
}

/**
 * `git commit -m <message>` against the staged index. Mirrors the
 * `git_commit_staged` Tauri command — hooks are *not* skipped.
 *
 * Throws when:
 * - `projectRoot` isn't a directory,
 * - `message` is empty after trimming,
 * - git itself returns non-zero (e.g. nothing staged, hook rejection).
 */
export async function gitCommitStaged(
  projectRoot: string,
  message: string,
): Promise<void> {
  return invoke("git_commit_staged", { projectRoot, message });
}

/**
 * `git push origin <branch || HEAD>`. Pass `force=true` to add `--force` —
 * the `/push` slash command only does this when the user explicitly types
 * `--force`. Returns a structured result; never throws on a non-zero git
 * exit (callers inspect `result.ok`).
 *
 * Throws only when the spawn itself fails (git missing, project root not a
 * directory, etc.) or when the branch arg looks like a flag.
 */
export async function gitPush(
  projectRoot: string,
  branch: string | null = null,
  force = false,
): Promise<PushResult> {
  return invoke<PushResult>("git_push", {
    projectRoot,
    branch,
    force,
  });
}

/**
 * Compact one-line render of a [`PushResult`] for inline display. Returns
 * stderr when present (push reports progress + status there), falling back
 * to stdout, then a generic exit-code string.
 */
export function summarizePushResult(r: PushResult): string {
  const last = (s: string) => {
    const lines = s.split(/\r?\n/).filter((l) => l.trim());
    return lines[lines.length - 1] ?? "";
  };
  const tail = last(r.stderr_tail) || last(r.stdout_tail);
  if (tail) return tail;
  return r.ok ? `exit 0` : `exit ${r.exit_code}`;
}
