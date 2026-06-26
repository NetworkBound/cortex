import { invoke } from "@tauri-apps/api/core";

export interface VaultFolder {
  path: string;
  note_count: number;
  total_count: number;
}

export interface VaultTag {
  tag: string;
  count: number;
}

export interface VaultNote {
  path: string;
  title: string;
  folder: string;
  tags: string[];
  link_count: number;
  backlink_count: number;
  size: number;
  is_orphan: boolean;
}

export interface VaultAnalysis {
  total_notes: number;
  total_folders: number;
  total_tags: number;
  orphan_count: number;
  broken_link_count: number;
  folders: VaultFolder[];
  tags: VaultTag[];
  notes: VaultNote[];
  broken_links: [string, string][];
}

export async function analyzeVault(
  vaultPath?: string,
): Promise<VaultAnalysis> {
  return invoke<VaultAnalysis>("analyze_vault", {
    vaultPath: vaultPath ?? null,
  });
}
