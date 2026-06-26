import { invoke } from "@tauri-apps/api/core";

/**
 * Thin Tauri-bridge wrappers for the IDE export backend command. Mirrors the
 * Rust `ExportResult` shape exactly so the modal can render results without
 * any post-processing.
 */

export interface ExportSkip {
  path: string;
  reason: string;
}

export interface ExportResult {
  written: string[];
  skipped: ExportSkip[];
}

/** All format ids the backend understands. Keep in sync with `KNOWN_FORMATS`. */
export const IDE_FORMATS = [
  { id: "cursor", label: "Cursor", target: ".cursor/rules/cortex.mdc" },
  { id: "windsurf", label: "Windsurf", target: ".windsurfrules" },
  { id: "cline", label: "Cline", target: ".clinerules/cortex.md" },
  { id: "copilot", label: "GitHub Copilot", target: ".github/copilot-instructions.md" },
  { id: "codex", label: "Codex (global)", target: "~/.codex/AGENTS.md" },
] as const;

export type IDEFormatId = (typeof IDE_FORMATS)[number]["id"];

export async function exportIDEConfigs(
  projectRoot: string,
  formats: IDEFormatId[],
): Promise<ExportResult> {
  return invoke<ExportResult>("export_ide_configs", { projectRoot, formats });
}
