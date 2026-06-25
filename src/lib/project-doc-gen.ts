/**
 * Thin TS wrapper around the `generate_project_doc` Tauri command.
 *
 * Mirrors `src-tauri::commands::project_doc_gen::ProjectDocResult`. The
 * backend caps the assembled context at 16 KiB and times out the gateway
 * call at 45s — callers should surface the rejection message verbatim.
 */
import { invoke } from "@tauri-apps/api/core";

/** Canonical doc types understood by the backend `canonicalize_doc_type`. */
export type ProjectDocType = "readme" | "claude-md" | "contributing";

/**
 * Mirrors `src-tauri::commands::project_doc_gen::ProjectDocResult`.
 * `suggested_path` is `<project_root>/README.md` (or CLAUDE.md /
 * CONTRIBUTING.md) using forward slashes, suitable for the `save_file_text`
 * Tauri command without further munging.
 */
export interface ProjectDocResult {
  doc_type: ProjectDocType;
  markdown: string;
  suggested_path: string;
  generated_unix_ms: number;
}

/**
 * Ask the gateway for a polished project-level doc of the given type. The backend
 * stitches together: project name + manifest (package.json / Cargo.toml /
 * pyproject.toml) + recent commits + top-level tree + any existing doc, all
 * capped at 16 KiB total.
 *
 * @param projectRoot Absolute path to the project root directory.
 * @param docType One of "readme" | "claude-md" | "contributing".
 */
export async function generateProjectDoc(
  projectRoot: string,
  docType: ProjectDocType,
): Promise<ProjectDocResult> {
  return invoke<ProjectDocResult>("generate_project_doc", {
    projectRoot,
    docType,
  });
}
