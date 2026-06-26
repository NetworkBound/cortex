import { invoke } from "@tauri-apps/api/core";

export interface ProjectMeta {
  root: string;
  name: string;
  has_claude_md: boolean;
  has_git: boolean;
  has_runbooks: boolean;
  last_modified_ms: number;
  group: string;
  kind: string;
  note_path: string | null;
  subtitle: string | null;
}

export interface FileTreeEntry {
  path: string;
  name: string;
  is_dir: boolean;
  size_bytes: number | null;
}

export async function listProjects(): Promise<ProjectMeta[]> {
  return invoke<ProjectMeta[]>("list_projects");
}

export async function setActiveProject(path: string): Promise<void> {
  return invoke("set_active_project", { path });
}

export async function projectFiles(path: string, limit = 500): Promise<FileTreeEntry[]> {
  return invoke<FileTreeEntry[]>("project_files", { path, limit });
}

/** Read a vault project note's markdown (confined to the vault root, capped
 *  at ~200 KB) for injection into chat as context. */
export async function openVaultNote(path: string): Promise<string> {
  return invoke<string>("open_vault_note", { path });
}
