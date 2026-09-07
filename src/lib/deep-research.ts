// In-app Deep Research — frontend bindings.
//
// Mirrors `src-tauri/src/commands/deep_research.rs`. Runs a multi-step
// research pipeline (plan → search → read → synthesize → save), streaming
// progress over `deep_research:progress`, and lists/reads saved reports from
// the vault's `research/` dir.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ResearchProgress {
  step: string;
  status: string;
  message: string | null;
  pct: number;
}

export interface ResearchSource {
  n: number;
  title: string;
  url: string;
}

export interface ResearchReport {
  question: string;
  markdown: string;
  sources: ResearchSource[];
  saved_path: string | null;
}

export interface SavedReport {
  title: string;
  path: string;
  question: string;
  created_unix_ms: number;
}

/** The research run currently in flight backend-side, if any. */
export interface ActiveResearch {
  question: string;
  progress: ResearchProgress;
}

export async function deepResearch(question: string, maxSources = 5): Promise<ResearchReport> {
  return invoke<ResearchReport>("deep_research", { question, maxSources });
}

/**
 * Snapshot of the research run currently in flight. Queried by the job store
 * on boot so a webview reload mid-run re-adopts the work instead of orphaning
 * it (the original invoke() belonged to the previous webview).
 */
export async function deepResearchActive(): Promise<ActiveResearch | null> {
  return invoke<ActiveResearch | null>("deep_research_active");
}

export async function listResearchReports(): Promise<SavedReport[]> {
  return invoke<SavedReport[]>("list_research_reports");
}

export async function readResearchReport(path: string): Promise<string> {
  return invoke<string>("read_research_report", { path });
}

export async function onResearchProgress(cb: (p: ResearchProgress) => void): Promise<UnlistenFn> {
  return listen<ResearchProgress>("deep_research:progress", (e) => cb(e.payload));
}

// NOTE: the old pending-question/window-event hand-off (`requestResearch`)
// is gone — `/research <question>` now calls `startDeepResearch` in
// `state/jobs.ts` directly. The run lives in module scope there, so it no
// longer matters whether the ResearchPanel is mounted when it starts.
