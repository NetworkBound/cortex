import { invoke } from "@tauri-apps/api/core";

/**
 * Frontend bindings for the `git_stash_*` Tauri command family that powers
 * the `/stash` modal. Mirrors the Rust structs one-for-one — keep the
 * shape in sync with `src-tauri/src/commands/git_stash.rs` when extending.
 */

/** Mirrors the Rust `Stash` struct. */
export interface Stash {
  /** Git stash ref, e.g. `stash@{0}`. */
  ref_id: string;
  /** Subject line, typically `WIP on <branch>: …` or the `-m` message. */
  subject: string;
  /** Relative-time string from git (`%cr`). */
  age: string;
  /** Best-effort count of files touched. `0` when parsing failed. */
  files_changed: number;
}

/** Mirrors the Rust `StashOpResult` struct. */
export interface StashOpResult {
  ok: boolean;
  stdout_tail: string;
  stderr_tail: string;
  exit_code: number;
}

/** `git stash list` parsed into structured rows. */
export async function gitStashList(projectRoot: string): Promise<Stash[]> {
  return invoke<Stash[]>("git_stash_list", { projectRoot });
}

/** `git stash apply <ref>`. */
export async function gitStashApply(
  projectRoot: string,
  refId: string,
): Promise<StashOpResult> {
  return invoke<StashOpResult>("git_stash_apply", { projectRoot, refId });
}

/** `git stash pop <ref>`. */
export async function gitStashPop(
  projectRoot: string,
  refId: string,
): Promise<StashOpResult> {
  return invoke<StashOpResult>("git_stash_pop", { projectRoot, refId });
}

/** `git stash drop <ref>`. */
export async function gitStashDrop(
  projectRoot: string,
  refId: string,
): Promise<StashOpResult> {
  return invoke<StashOpResult>("git_stash_drop", { projectRoot, refId });
}

/**
 * `git stash push [-m <msg>] [--include-untracked]`.
 * Both arguments are optional; pass `null` / `false` to skip.
 */
export async function gitStashSave(
  projectRoot: string,
  message: string | null,
  includeUntracked: boolean,
): Promise<StashOpResult> {
  return invoke<StashOpResult>("git_stash_save", {
    projectRoot,
    message,
    includeUntracked,
  });
}

/**
 * `git stash show -p <ref>` — truncated diff text for the Diff button.
 * Backend caps the return at 32 KiB with a trailing comment line.
 */
export async function gitStashShow(
  projectRoot: string,
  refId: string,
): Promise<string> {
  return invoke<string>("git_stash_show", { projectRoot, refId });
}

/**
 * One-line summary of a [`StashOpResult`] suitable for a toast or system
 * note. Returns the last non-empty line of stderr (git surfaces status
 * there), falling back to stdout, then an exit-code fallback.
 */
export function summarizeStashOp(r: StashOpResult): string {
  const last = (s: string) => {
    const lines = s.split(/\r?\n/).filter((l) => l.trim());
    return lines[lines.length - 1] ?? "";
  };
  const tail = last(r.stderr_tail) || last(r.stdout_tail);
  if (tail) return tail;
  return r.ok ? "exit 0" : `exit ${r.exit_code}`;
}
