import { invoke } from "@tauri-apps/api/core";

export interface MemoryFile {
  path: string;
  name: string;
  size_bytes: number;
  source: string;
  source_kind: string;
  modified_unix_ms: number;
}

export interface MarkdownEntry {
  path: string;
  title: string | null;
  frontmatter: Record<string, unknown>;
  body: string;
  wikilinks: string[];
  size_bytes: number;
  modified_unix_ms: number;
}

export interface MemorySearchHit {
  source: string;
  path: string;
  snippet: string;
  score: number;
}

export async function listMemoryFiles(activeProject?: string, obsidianVault?: string): Promise<MemoryFile[]> {
  return invoke<MemoryFile[]>("list_memory_files", {
    activeProject: activeProject ?? null,
    obsidianVault: obsidianVault ?? null,
  });
}

export async function getMemoryEntry(path: string): Promise<MarkdownEntry> {
  return invoke<MarkdownEntry>("get_memory_entry", { path });
}

export async function searchMemory(
  query: string,
  opts: { activeProject?: string; obsidianVault?: string; includeChroma?: boolean } = {},
): Promise<MemorySearchHit[]> {
  return invoke<MemorySearchHit[]>("search_memory", {
    query,
    activeProject: opts.activeProject ?? null,
    obsidianVault: opts.obsidianVault ?? null,
    includeChroma: opts.includeChroma ?? false,
  });
}

export async function writeMemoryEntry(path: string, content: string): Promise<void> {
  await invoke<void>("write_memory_entry", { path, content });
}

export async function createMemoryEntry(name: string, content: string, projectRoot?: string): Promise<string> {
  const slug =
    name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").slice(0, 60).replace(/^-+|-+$/g, "") ||
    `pinned-${Date.now()}`;
  const relPath = `runbooks/pinned-${slug}.md`;
  const fullPath = projectRoot ? `${projectRoot.replace(/[/\\]$/, "")}/${relPath}` : relPath;
  const body = `# ${name}\n\n${content}\n`;
  await invoke<void>("create_memory_entry", { path: fullPath, content: body });
  return fullPath;
}
