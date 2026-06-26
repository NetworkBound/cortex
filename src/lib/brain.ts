import { invoke } from "@tauri-apps/api/core";

export interface BrainSnapshot {
  recent_projects: RecentProject[];
  recent_sessions: RecentSession[];
  recent_memory: RecentMemory[];
  obsidian_vault: string | null;
}

export interface RecentProject {
  root: string;
  name: string;
  last_modified_ms: number;
  has_git: boolean;
  has_claude_md: boolean;
  has_runbooks: boolean;
}

export interface RecentSession {
  session_id: string;
  last_active_ms: number;
  message_count: number;
  agents: string[];
  first_message: string | null;
}

export interface RecentMemory {
  path: string;
  title: string | null;
  source: string;
  modified_unix_ms: number;
  preview: string;
}

export async function brainSnapshot(): Promise<BrainSnapshot> {
  return invoke<BrainSnapshot>("brain_snapshot");
}

export async function setObsidianVault(path: string | null): Promise<void> {
  return invoke("set_obsidian_vault", { path });
}
