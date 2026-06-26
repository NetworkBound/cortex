//! Local-model Cookbook — hardware-aware local-model recommendations +
//! one-click serving via Ollama.
//!
//! Detects the host's CPU/RAM/GPU, ranks a curated catalog of local models by
//! what actually fits the machine, and pulls a chosen model into the local
//! Ollama server (streaming `/api/pull` progress over a Tauri event). Surfaces
//! the already-installed tags so the UI can show what's served.
//!
//! Best-effort and honest about a missing/stopped Ollama: this is a homelab
//! box where Ollama may not be installed or running, so the specs carry
//! `ollama_installed`/`ollama_running` flags and the pull command returns a
//! clear, actionable error rather than a stack trace. The parsing/ranking
//! helpers are pure functions so they're unit-tested without a live server.

use crate::app_state::AppState;
use futures::StreamExt;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use tauri::{Emitter, State};

/// In-flight pull registry: model name → latest progress. Lets the frontend
/// job store (a) refuse a double pull while one is already streaming and
/// (b) re-adopt running pulls after a webview reload via
/// `cookbook_active_pulls` — the pull itself outlives the invoke() that
/// started it only backend-side, so this is the source of truth for "what is
/// pulling right now".
static ACTIVE_PULLS: Lazy<Mutex<HashMap<String, PullProgress>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Serialize)]
pub struct HostSpecs {
    pub cpu_cores: usize,
    pub ram_total_mb: u64,
    pub ram_avail_mb: u64,
    pub gpu_name: Option<String>,
    pub vram_total_mb: Option<u64>,
    pub has_cuda: bool,
    pub ollama_installed: bool,
    pub ollama_running: bool,
    pub ollama_base_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRec {
    pub name: String,
    pub label: String,
    pub tier: String,
    pub params_b: f32,
    pub download_gb: f32,
    pub min_ram_gb: f32,
    pub recommended_vram_gb: f32,
    pub fits: bool,
    pub fit_reason: String,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CookbookView {
    pub specs: HostSpecs,
    pub recommendations: Vec<ModelRec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PullProgress {
    pub name: String,
    pub status: String,
    pub completed: u64,
    pub total: u64,
    pub pct: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PullResult {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

struct CatalogItem {
    name: &'static str,
    label: &'static str,
    tier: &'static str,
    params_b: f32,
    download_gb: f32,
    min_ram_gb: f32,
    recommended_vram_gb: f32,
}

// Curated set of popular Ollama tags spanning size/role. `min_ram_gb` is the
// rough working-set to run quantized on CPU; `recommended_vram_gb` is the GPU
// path (0 = comfortably CPU-friendly). Kept deliberately small + opinionated.
const CATALOG: &[CatalogItem] = &[
    CatalogItem { name: "llama3.2:1b",        label: "Llama 3.2 1B",        tier: "general",   params_b: 1.0,  download_gb: 1.3, min_ram_gb: 3.0,  recommended_vram_gb: 0.0 },
    CatalogItem { name: "qwen2.5-coder:1.5b", label: "Qwen2.5 Coder 1.5B",  tier: "coding",    params_b: 1.5,  download_gb: 1.0, min_ram_gb: 4.0,  recommended_vram_gb: 0.0 },
    CatalogItem { name: "deepseek-r1:1.5b",   label: "DeepSeek-R1 1.5B",    tier: "reasoning", params_b: 1.5,  download_gb: 1.1, min_ram_gb: 4.0,  recommended_vram_gb: 0.0 },
    CatalogItem { name: "gemma2:2b",          label: "Gemma 2 2B",          tier: "general",   params_b: 2.0,  download_gb: 1.6, min_ram_gb: 5.0,  recommended_vram_gb: 0.0 },
    CatalogItem { name: "llama3.2:3b",        label: "Llama 3.2 3B",        tier: "general",   params_b: 3.0,  download_gb: 2.0, min_ram_gb: 6.0,  recommended_vram_gb: 0.0 },
    CatalogItem { name: "qwen2.5:7b",         label: "Qwen 2.5 7B",         tier: "general",   params_b: 7.0,  download_gb: 4.7, min_ram_gb: 9.0,  recommended_vram_gb: 6.0 },
    CatalogItem { name: "qwen2.5-coder:7b",   label: "Qwen2.5 Coder 7B",    tier: "coding",    params_b: 7.0,  download_gb: 4.7, min_ram_gb: 9.0,  recommended_vram_gb: 6.0 },
    CatalogItem { name: "deepseek-r1:7b",     label: "DeepSeek-R1 7B",      tier: "reasoning", params_b: 7.0,  download_gb: 4.7, min_ram_gb: 9.0,  recommended_vram_gb: 6.0 },
    CatalogItem { name: "mistral:7b",         label: "Mistral 7B",          tier: "general",   params_b: 7.0,  download_gb: 4.1, min_ram_gb: 9.0,  recommended_vram_gb: 6.0 },
    CatalogItem { name: "llava:7b",           label: "LLaVA 7B (vision)",   tier: "vision",    params_b: 7.0,  download_gb: 4.7, min_ram_gb: 9.0,  recommended_vram_gb: 6.0 },
    CatalogItem { name: "llama3.1:8b",        label: "Llama 3.1 8B",        tier: "general",   params_b: 8.0,  download_gb: 4.9, min_ram_gb: 10.0, recommended_vram_gb: 6.0 },
    CatalogItem { name: "qwen2.5:14b",        label: "Qwen 2.5 14B",        tier: "general",   params_b: 14.0, download_gb: 9.0, min_ram_gb: 18.0, recommended_vram_gb: 12.0 },
];

// ----- pure helpers (unit-tested without a live host/server) -----

/// Parse `/proc/meminfo` into `(total_mb, avail_mb)`.
fn parse_meminfo(contents: &str) -> (u64, u64) {
    let field = |key: &str| -> u64 {
        contents
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    };
    (field("MemTotal:"), field("MemAvailable:"))
}

/// Parse `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`
/// → `(name, vram_mb)` from the first non-empty line.
fn parse_nvidia_smi(out: &str) -> Option<(String, u64)> {
    let line = out.lines().find(|l| !l.trim().is_empty())?;
    let mut parts = line.splitn(2, ',');
    let name = parts.next()?.trim().to_string();
    let vram = parts.next()?.trim().parse::<u64>().ok()?;
    if name.is_empty() {
        return None;
    }
    Some((name, vram))
}

/// Pull a human GPU name out of an `lspci` dump (the VGA/3D/display line).
fn parse_lspci_vga(out: &str) -> Option<String> {
    let line = out.lines().find(|l| {
        let low = l.to_lowercase();
        low.contains("vga compatible controller")
            || low.contains("3d controller")
            || low.contains("display controller")
    })?;
    // Keep everything after the FIRST ": " (device strings can contain more).
    let name = line.splitn(2, ": ").nth(1)?.trim();
    // Drop a trailing "(rev 01)" revision suffix for a cleaner label.
    let name = name.split(" (rev ").next().unwrap_or(name).trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Parse one Ollama `/api/pull` NDJSON progress line into a `PullProgress`.
fn parse_pull_line(name: &str, line: &str) -> Option<PullProgress> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let status = v.get("status").and_then(|s| s.as_str())?.to_string();
    let total = v.get("total").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let completed = v.get("completed").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let pct = if total > 0 {
        ((completed as f32 / total as f32) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    Some(PullProgress { name: name.to_string(), status, completed, total, pct })
}

/// Rank the catalog against the host: fitting models first (installed bubble to
/// the very top), then smallest-first within a bucket so the safe pick leads.
fn rank_catalog(specs: &HostSpecs, installed: &[String]) -> Vec<ModelRec> {
    let ram_gb = specs.ram_avail_mb as f32 / 1024.0;
    let vram_gb = specs.vram_total_mb.map(|m| m as f32 / 1024.0).unwrap_or(0.0);
    let mut recs: Vec<ModelRec> = CATALOG
        .iter()
        .map(|c| {
            let installed_flag = installed.iter().any(|t| t == c.name);
            // With a GPU present, anything within VRAM fits the GPU path —
            // including the tiny models whose recommended VRAM is 0.
            let gpu_fits = vram_gb > 0.0 && vram_gb >= c.recommended_vram_gb;
            let cpu_fits = ram_gb >= c.min_ram_gb;
            let (fits, fit_reason) = if gpu_fits {
                (true, format!("fits your {vram_gb:.0} GB GPU"))
            } else if cpu_fits {
                (true, format!("runs on CPU — {ram_gb:.0} GB free, needs ~{:.0}", c.min_ram_gb))
            } else {
                (false, format!("needs ~{:.0} GB RAM (you have {ram_gb:.0} free)", c.min_ram_gb))
            };
            ModelRec {
                name: c.name.to_string(),
                label: c.label.to_string(),
                tier: c.tier.to_string(),
                params_b: c.params_b,
                download_gb: c.download_gb,
                min_ram_gb: c.min_ram_gb,
                recommended_vram_gb: c.recommended_vram_gb,
                fits,
                fit_reason,
                installed: installed_flag,
            }
        })
        .collect();
    recs.sort_by(|a, b| {
        b.installed
            .cmp(&a.installed)
            .then(b.fits.cmp(&a.fits))
            .then(a.params_b.partial_cmp(&b.params_b).unwrap_or(std::cmp::Ordering::Equal))
    });
    recs
}

// ----- host / server probes -----

fn detect_ram_mb() -> (u64, u64) {
    std::fs::read_to_string("/proc/meminfo")
        .map(|c| parse_meminfo(&c))
        .unwrap_or((0, 0))
}

fn detect_gpu() -> (Option<String>, Option<u64>, bool) {
    if let Ok(out) = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .output()
    {
        if out.status.success() {
            if let Some((name, vram)) = parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout)) {
                return (Some(name), Some(vram), true);
            }
        }
    }
    if let Ok(out) = std::process::Command::new("lspci").output() {
        if out.status.success() {
            if let Some(name) = parse_lspci_vga(&String::from_utf8_lossy(&out.stdout)) {
                return (Some(name), None, false);
            }
        }
    }
    (None, None, false)
}

fn ollama_on_path() -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join("ollama").is_file()))
        .unwrap_or(false)
}

async fn probe_ollama(base: &str) -> bool {
    if base.is_empty() {
        return false;
    }
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(2)).build() else {
        return false;
    };
    client
        .get(format!("{base}/api/tags"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn fetch_installed_tags(base: &str) -> Vec<String> {
    if base.is_empty() {
        return Vec::new();
    }
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(3)).build() else {
        return Vec::new();
    };
    let Ok(resp) = client.get(format!("{base}/api/tags")).send().await else {
        return Vec::new();
    };
    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return Vec::new();
    };
    json.get("models")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

async fn gather_specs(base: &str) -> HostSpecs {
    let cpu_cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let (ram_total_mb, ram_avail_mb) = detect_ram_mb();
    let (gpu_name, vram_total_mb, has_cuda) = detect_gpu();
    HostSpecs {
        cpu_cores,
        ram_total_mb,
        ram_avail_mb,
        gpu_name,
        vram_total_mb,
        has_cuda,
        ollama_installed: ollama_on_path(),
        ollama_running: probe_ollama(base).await,
        ollama_base_url: base.to_string(),
    }
}

use crate::agents::ollama::LOCAL_OLLAMA;

/// The Cookbook serves models on THIS machine (its hardware detection is all
/// local), so prefer a reachable local Ollama; fall back to the configured
/// (possibly remote homelab) endpoint when no local server is running.
async fn resolve_base(state: &State<'_, AppState>) -> String {
    if probe_ollama(LOCAL_OLLAMA).await {
        return LOCAL_OLLAMA.to_string();
    }
    state.config.read().ollama_base_url.trim_end_matches('/').to_string()
}

// ----- Tauri commands -----

#[tauri::command]
pub async fn cookbook_host_specs(state: State<'_, AppState>) -> Result<HostSpecs, String> {
    let base = resolve_base(&state).await;
    Ok(gather_specs(&base).await)
}

#[tauri::command]
pub async fn cookbook_recommendations(state: State<'_, AppState>) -> Result<CookbookView, String> {
    let base = resolve_base(&state).await;
    let specs = gather_specs(&base).await;
    let installed = fetch_installed_tags(&base).await;
    let recommendations = rank_catalog(&specs, &installed);
    Ok(CookbookView { specs, recommendations })
}

/// Snapshot of every pull currently in flight (latest progress per model).
/// The frontend job store queries this on boot so a webview reload mid-pull
/// re-adopts the running job instead of orphaning it.
#[tauri::command]
pub async fn cookbook_active_pulls() -> Result<Vec<PullProgress>, String> {
    Ok(ACTIVE_PULLS.lock().values().cloned().collect())
}

/// Pull a model into the local Ollama server, streaming progress to the
/// frontend over `cookbook:pull:<name>` events. Returns once the pull settles.
/// A second pull of the same model while one is streaming is rejected.
#[tauri::command]
pub async fn cookbook_pull_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<PullResult, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("No model name given.".into());
    }
    let base = resolve_base(&state).await;
    if base.is_empty() {
        return Err("No Ollama base URL configured. Set OLLAMA_BASE_URL.".into());
    }
    {
        let mut active = ACTIVE_PULLS.lock();
        if active.contains_key(&name) {
            return Err(format!("'{name}' is already being pulled."));
        }
        active.insert(
            name.clone(),
            PullProgress {
                name: name.clone(),
                status: "starting".into(),
                completed: 0,
                total: 0,
                pct: 0.0,
            },
        );
    }
    let result = run_pull(&app, &base, &name).await;
    ACTIVE_PULLS.lock().remove(&name);
    result
}

/// Per-model progress event name. Tauri event names only allow
/// `[a-zA-Z0-9-/:_]`, but Ollama tags routinely contain dots
/// (`llama3.2:1b`) — an unsanitized name made `emit` fail silently AND the
/// frontend `listen` reject, so per-model progress never flowed for dotted
/// tags. Must stay in lockstep with `pullEventName` in `src/lib/cookbook.ts`.
fn pull_event_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | ':' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("cookbook:pull:{safe}")
}

async fn run_pull(
    app: &tauri::AppHandle,
    base: &str,
    name: &str,
) -> Result<PullResult, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .post(format!("{base}/api/pull"))
        .json(&serde_json::json!({ "name": name, "stream": true }))
        .send()
        .await
        .map_err(|e| {
            format!("Ollama isn't reachable at {base} — install and start Ollama, then retry. ({e})")
        })?;
    if !resp.status().is_success() {
        return Err(format!("Ollama returned {} for pull of '{name}'.", resp.status()));
    }
    let event = pull_event_name(name);
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut last_status = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                    return Err(format!("Ollama pull failed: {err}"));
                }
            }
            if let Some(prog) = parse_pull_line(name, line) {
                last_status = prog.status.clone();
                if let Some(slot) = ACTIVE_PULLS.lock().get_mut(name) {
                    *slot = prog.clone();
                }
                let _ = app.emit(&event, &prog);
            }
        }
    }
    // The model universe just changed — let every open model surface
    // (composer picker, status-bar strip) refresh without a restart.
    let _ = app.emit("models:changed", &name);
    Ok(PullResult {
        name: name.to_string(),
        ok: true,
        message: format!("Pulled '{name}' ({last_status})"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(ram_avail_mb: u64, vram_mb: Option<u64>) -> HostSpecs {
        HostSpecs {
            cpu_cores: 8,
            ram_total_mb: 16000,
            ram_avail_mb,
            gpu_name: None,
            vram_total_mb: vram_mb,
            has_cuda: vram_mb.is_some(),
            ollama_installed: false,
            ollama_running: false,
            ollama_base_url: String::new(),
        }
    }

    #[test]
    fn pull_event_name_sanitizes_to_tauri_charset() {
        // Dots are the common offender in Ollama tags; anything outside
        // Tauri's allowed [a-zA-Z0-9-/:_] must be replaced, never dropped.
        assert_eq!(pull_event_name("llama3.2:1b"), "cookbook:pull:llama3_2:1b");
        assert_eq!(pull_event_name("qwen2.5:14b"), "cookbook:pull:qwen2_5:14b");
        assert_eq!(pull_event_name("mistral"), "cookbook:pull:mistral");
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | ':' | '_');
        assert!(pull_event_name("weird name+tag@v1.0").chars().all(allowed));
    }

    #[test]
    fn meminfo_parses_total_and_avail() {
        let sample = "MemTotal:       16123300 kB\nMemFree: 100 kB\nMemAvailable:    8000000 kB\n";
        assert_eq!(parse_meminfo(sample), (16123300 / 1024, 8000000 / 1024));
    }

    #[test]
    fn nvidia_smi_parses_name_and_vram() {
        assert_eq!(
            parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564\n"),
            Some(("NVIDIA GeForce RTX 4090".to_string(), 24564))
        );
        assert_eq!(parse_nvidia_smi("   \n"), None);
    }

    #[test]
    fn lspci_extracts_gpu_name_without_revision() {
        let sample = "00:1f.0 ISA bridge: Intel Corporation\n0000:00:02.0 VGA compatible controller: Intel Corporation TigerLake-LP GT2 [Iris Xe Graphics] (rev 01)\n";
        assert_eq!(
            parse_lspci_vga(sample).as_deref(),
            Some("Intel Corporation TigerLake-LP GT2 [Iris Xe Graphics]")
        );
        assert_eq!(parse_lspci_vga("no gpu here\n"), None);
    }

    #[test]
    fn pull_line_computes_percent() {
        let p = parse_pull_line("llama3.2:3b", "{\"status\":\"downloading\",\"total\":100,\"completed\":40}").unwrap();
        assert_eq!(p.status, "downloading");
        assert!((p.pct - 40.0).abs() < 0.01);
        // a status-only line (no total) is still valid at 0%.
        let p2 = parse_pull_line("x", "{\"status\":\"verifying sha256\"}").unwrap();
        assert_eq!(p2.pct, 0.0);
        // garbage / non-object → None.
        assert!(parse_pull_line("x", "not json").is_none());
    }

    #[test]
    fn ranking_fits_cpu_models_on_a_16gb_no_gpu_box() {
        // ~8 GB free, no GPU (this dev laptop's shape).
        let recs = rank_catalog(&specs(8000, None), &[]);
        let small = recs.iter().find(|r| r.name == "llama3.2:3b").unwrap();
        assert!(small.fits, "3B should fit 8GB free RAM");
        let big = recs.iter().find(|r| r.name == "qwen2.5:14b").unwrap();
        assert!(!big.fits, "14B (needs ~18GB) should not fit");
        // fitting models must sort ahead of non-fitting ones.
        let first_unfit = recs.iter().position(|r| !r.fits).unwrap();
        assert!(recs[..first_unfit].iter().all(|r| r.fits));
    }

    /// Live end-to-end check against a real local Ollama (run manually with
    /// `cargo test --lib cookbook -- --ignored`). Exercises the actual
    /// detection path — probe + installed-tag fetch + ranking — and asserts a
    /// pulled model is detected and bubbles to the top. Ignored by default so
    /// CI without Ollama stays green.
    #[tokio::test]
    #[ignore]
    async fn live_local_ollama_detects_installed_model() {
        assert!(probe_ollama(LOCAL_OLLAMA).await, "local ollama must be running on 11434");
        let specs = gather_specs(LOCAL_OLLAMA).await;
        assert!(specs.ollama_running, "specs should report ollama_running");
        let installed = fetch_installed_tags(LOCAL_OLLAMA).await;
        assert!(!installed.is_empty(), "expected at least one pulled model");
        let recs = rank_catalog(&specs, &installed);
        // an installed catalog model must be flagged installed and sort first
        if installed.iter().any(|t| CATALOG.iter().any(|c| c.name == t)) {
            assert!(recs[0].installed, "an installed model should bubble to the top");
        }
    }

    #[test]
    fn ranking_bubbles_installed_to_top_and_flags_gpu_fit() {
        // 24 GB GPU → the 7B/8B GPU-path models fit too.
        let recs = rank_catalog(&specs(8000, Some(24000)), &["mistral:7b".to_string()]);
        assert_eq!(recs[0].name, "mistral:7b", "installed model sorts first");
        assert!(recs[0].installed);
        assert!(recs.iter().find(|r| r.name == "qwen2.5:7b").unwrap().fits);
    }
}
