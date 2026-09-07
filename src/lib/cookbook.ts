// Local-model Cookbook — frontend bindings.
//
// Mirrors `src-tauri/src/commands/cookbook.rs` ({HostSpecs, ModelRec,
// CookbookView, PullProgress, PullResult}). Detects host hardware, ranks a
// curated catalog of local models by what fits, and pulls a model into the
// local Ollama server (progress streamed over `cookbook:pull:<name>` events).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface HostSpecs {
  cpu_cores: number;
  ram_total_mb: number;
  ram_avail_mb: number;
  gpu_name: string | null;
  vram_total_mb: number | null;
  has_cuda: boolean;
  ollama_installed: boolean;
  ollama_running: boolean;
  ollama_base_url: string;
}

export interface ModelRec {
  name: string;
  label: string;
  tier: string;
  params_b: number;
  download_gb: number;
  min_ram_gb: number;
  recommended_vram_gb: number;
  fits: boolean;
  fit_reason: string;
  installed: boolean;
}

export interface CookbookView {
  specs: HostSpecs;
  recommendations: ModelRec[];
}

export interface PullProgress {
  name: string;
  status: string;
  completed: number;
  total: number;
  pct: number;
}

export interface PullResult {
  name: string;
  ok: boolean;
  message: string;
}

export async function hostSpecs(): Promise<HostSpecs> {
  return invoke<HostSpecs>("cookbook_host_specs");
}

export async function recommendations(): Promise<CookbookView> {
  return invoke<CookbookView>("cookbook_recommendations");
}

export async function pullModel(name: string): Promise<PullResult> {
  return invoke<PullResult>("cookbook_pull_model", { name });
}

/**
 * Per-model progress event name. Tauri event names only allow
 * `[a-zA-Z0-9-/:_]`, but Ollama tags routinely contain dots (`llama3.2:1b`),
 * so the raw tag must be sanitized exactly like the backend does — must stay
 * in lockstep with `pull_event_name` in `src-tauri/src/commands/cookbook.rs`.
 */
function pullEventName(name: string): string {
  return `cookbook:pull:${name.replace(/[^a-zA-Z0-9/:_-]/g, "_")}`;
}

/** Subscribe to pull-progress events for one model. Returns an unlisten fn. */
export async function onPullProgress(
  name: string,
  cb: (p: PullProgress) => void,
): Promise<UnlistenFn> {
  return listen<PullProgress>(pullEventName(name), (e) => cb(e.payload));
}
