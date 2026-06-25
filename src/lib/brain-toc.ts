import { invoke } from "@tauri-apps/api/core";

/**
 * Frontend mirrors of the `brain_toc` Tauri command response.
 *
 * One JSON blob, walked once from the backend, holds every memory source
 * (Claude project memory, runbooks, Obsidian, project / global instructions)
 * grouped under a stable snake-case kind string. The modal renders these
 * verbatim — no per-file backend round-trips on click.
 */

export interface TocHeading {
  /** `#` count — 1..6. */
  level: number;
  text: string;
  /** 1-based line number in the source file (for editor scroll hints). */
  line: number;
}

export interface TocFile {
  path: string;
  /** First `# heading` if present, otherwise the filename stem. */
  title: string;
  headings: TocHeading[];
}

export type TocKind = "claude" | "runbooks" | "obsidian" | "project" | "global";

export interface TocSource {
  kind: TocKind | string;
  label: string;
  files: TocFile[];
}

export interface TocResult {
  sources: TocSource[];
  file_count: number;
  heading_count: number;
  /** True when the 500-file cap was hit. */
  truncated: boolean;
}

/** Walk every memory source and return its TOC. */
export async function brainToc(): Promise<TocResult> {
  return invoke<TocResult>("brain_toc");
}

/** Pretty label for the source kind. Used in modal group headers. */
export function kindLabel(kind: string): string {
  switch (kind) {
    case "claude":
      return "Claude project memory";
    case "runbooks":
      return "Runbooks";
    case "obsidian":
      return "Obsidian";
    case "project":
      return "Project instructions";
    case "global":
      return "Global instructions";
    default:
      return kind;
  }
}

/** Render the whole TOC as plain markdown for clipboard copy. */
export function formatTocAsMarkdown(result: TocResult): string {
  const out: string[] = ["# Cortex Brain — table of contents", ""];
  for (const src of result.sources) {
    out.push(`## ${kindLabel(src.kind)} — ${src.label}`);
    out.push("");
    for (const file of src.files) {
      out.push(`### ${file.title}`);
      out.push(`\`${file.path}\``);
      out.push("");
      for (const h of file.headings) {
        const indent = "  ".repeat(Math.max(0, h.level - 1));
        out.push(`${indent}- ${h.text}`);
      }
      out.push("");
    }
  }
  return out.join("\n");
}
