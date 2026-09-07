import { invoke } from "@tauri-apps/api/core";

export interface Commit {
  hash: string;
  short_hash: string;
  author: string;
  age: string;
  subject: string;
  refs: string[];
  parents: string[];
}

export interface FileEntry {
  path: string;
  status: string;
}

export interface CommitFile {
  path: string;
  status: string;
}

export interface WorkingStatus {
  branch: string;
  ahead: number;
  behind: number;
  staged: FileEntry[];
  unstaged: FileEntry[];
  untracked: string[];
}

export async function gitHistory(
  projectRoot: string,
  limit = 100,
  offset = 0,
): Promise<Commit[]> {
  return invoke<Commit[]>("git_history", { projectRoot, limit, offset });
}

export async function gitShow(
  projectRoot: string,
  hash: string,
): Promise<string> {
  return invoke<string>("git_show", { projectRoot, hash });
}

/** Files changed in a single commit (status + path). */
export async function gitCommitFiles(
  projectRoot: string,
  hash: string,
): Promise<CommitFile[]> {
  return invoke<CommitFile[]>("git_commit_files", { projectRoot, hash });
}

/** Unified diff for one file within a commit. */
export async function gitCommitFileDiff(
  projectRoot: string,
  hash: string,
  path: string,
): Promise<string> {
  return invoke<string>("git_commit_file_diff", { projectRoot, hash, path });
}

export async function gitWorkingStatus(
  projectRoot: string,
): Promise<WorkingStatus> {
  return invoke<WorkingStatus>("git_working_status", { projectRoot });
}

export async function gitStageFile(
  projectRoot: string,
  path: string,
): Promise<void> {
  return invoke("git_stage_file", { projectRoot, path });
}

export async function gitUnstageFile(
  projectRoot: string,
  path: string,
): Promise<void> {
  return invoke("git_unstage_file", { projectRoot, path });
}

export async function gitDiscardChanges(
  projectRoot: string,
  path: string,
): Promise<void> {
  return invoke("git_discard_changes", { projectRoot, path });
}

/** Which side of the index to diff for `gitFileDiff`. */
export type DiffMode = "staged" | "unstaged" | "untracked";

/**
 * Unified diff text for one file. `staged` compares index↔HEAD, `unstaged`
 * compares working-tree↔index, and `untracked` synthesizes an all-additions
 * patch so new files are reviewable too. Output is capped server-side.
 */
export async function gitFileDiff(
  projectRoot: string,
  path: string,
  mode: DiffMode,
): Promise<string> {
  return invoke<string>("git_file_diff", { projectRoot, path, mode });
}

export async function gitCommit(
  projectRoot: string,
  message: string,
): Promise<void> {
  return invoke("git_commit", { projectRoot, message });
}
