import { invoke } from "@tauri-apps/api/core";

/**
 * Diagnostics export binding. The backend bundles app version + build
 * variant, OS info, the crash log, recent session *metadata* (never message
 * contents) and a fully redacted config snapshot into a single
 * `~/.cortex/diagnostics-<ts>.tar.gz` the user can attach to a bug report.
 *
 * Everything inside the archive passes the backend redactor (keys, tokens,
 * private/tailnet IPs, home paths) — see
 * `src-tauri/src/commands/diagnostics.rs`.
 */
export interface DiagnosticsExport {
  /** Absolute path of the written archive. */
  path: string;
  /** Names of the files inside the archive. */
  files: string[];
}

export async function exportDiagnostics(): Promise<DiagnosticsExport> {
  return invoke<DiagnosticsExport>("export_diagnostics");
}
