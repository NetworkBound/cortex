import { invoke } from "@tauri-apps/api/core";

export interface Worktree {
  id: string;
  project_root: string;
  branch: string;
  path: string;
  session_id: string | null;
  created_at: number;
  archived_at: number | null;
  status: "active" | "archived";
  notes: string | null;
}

export async function listWorktrees(projectRoot?: string): Promise<Worktree[]> {
  return invoke<Worktree[]>("list_worktrees", {
    projectRoot: projectRoot ?? null,
  });
}

export async function createWorktree(
  projectRoot: string,
  note?: string,
): Promise<Worktree> {
  return invoke<Worktree>("create_worktree", {
    args: { project_root: projectRoot, note: note ?? null },
  });
}

export async function removeWorktree(
  id: string,
  archiveCommit = true,
): Promise<void> {
  return invoke("remove_worktree", {
    args: { id, archive_commit: archiveCommit },
  });
}
