import { invoke } from "@tauri-apps/api/core";

/**
 * Mirrors `src-tauri::commands::shell_run::ShellResult`. `exit_code` is
 * `null` when the command timed out or was killed before completing.
 */
export interface ShellResult {
  exit_code: number | null;
  stdout: string;
  stderr: string;
  duration_ms: number;
  truncated: boolean;
  timed_out: boolean;
}

/**
 * Run a user-typed shell command via the Tauri backend.
 *
 * Backend caps output at 16 KiB and the wall-clock at 30s; results that
 * exceed either are returned with the matching flag set so the UI can
 * surface a "[truncated]" / "[timed out]" hint instead of pretending the
 * snapshot is complete.
 */
export async function shellExec(
  cmd: string,
  projectRoot?: string | null,
): Promise<ShellResult> {
  return invoke<ShellResult>("shell_exec", {
    args: { cmd, project_root: projectRoot ?? null },
  });
}

/**
 * Render a `ShellResult` as a chat-message-friendly markdown block. We
 * truncate stdout/stderr to `maxLines` lines combined so the chat scroll
 * doesn't blow out on a `find /` accident — the backend's byte cap
 * doesn't account for line-count, only total bytes.
 */
export function formatShellResult(cmd: string, r: ShellResult, maxLines = 64): string {
  const header = r.timed_out
    ? `\`$ ${cmd}\` — _timed out after ${r.duration_ms}ms_`
    : `\`$ ${cmd}\` — exit ${r.exit_code ?? "?"} (${r.duration_ms}ms)`;
  const body = clipLines([r.stdout, r.stderr].filter(Boolean).join("\n"), maxLines);
  const truncHint = r.truncated ? "\n\n_…output truncated (16 KiB cap)._" : "";
  if (!body.trim()) return `${header}\n_(no output)_${truncHint}`;
  return `${header}\n\n\`\`\`\n${body}\n\`\`\`${truncHint}`;
}

function clipLines(s: string, maxLines: number): string {
  const lines = s.split("\n");
  if (lines.length <= maxLines) return s;
  return `${lines.slice(0, maxLines).join("\n")}\n…(+${lines.length - maxLines} more lines)`;
}
