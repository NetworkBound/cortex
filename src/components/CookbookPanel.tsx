/**
 * Local-model Cookbook panel.
 *
 * Hardware-aware local-model recommendations + one-click serving via Ollama.
 * Top: a host-specs card (CPU / RAM / GPU + Ollama status). Below: the curated
 * catalog ranked by what fits this machine — installed models bubble to the
 * top, fitting models next, smallest-first within a bucket. Each row pulls the
 * model into the local Ollama server with a live progress bar; the pull itself
 * is owned by the global job store (`state/jobs.ts`), so it survives switching
 * away from this tab and reports completion to the StatusBar/NotificationCenter.
 * Installed rows offer a one-click "Use in chat" hand-off that selects
 * `ollama:<tag>` in the composer's ModelPicker and jumps to the chat.
 *
 * Honest about a missing/stopped Ollama: when it isn't running the pull buttons
 * are disabled and a callout explains how to get it going, rather than failing
 * on click. Bindings live in `src/lib/cookbook.ts`.
 */

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Cpu, HardDrive, MonitorCog, Download, Check, MessageSquare } from "lucide-react";
import { PanelLoading } from "./Skeleton";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import { startCookbookPull, useJobs } from "@/state/jobs";
import {
  recommendations as fetchRecommendations,
  type CookbookView,
  type ModelRec,
} from "@/lib/cookbook";
import "../styles/cookbook.css";

function gb(mb: number): string {
  return `${(mb / 1024).toFixed(1)} GB`;
}

export function CookbookPanel() {
  const [view, setView] = useState<CookbookView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // In-flight pulls live in the GLOBAL job store (state/jobs.ts), not local
  // state — they must survive this panel unmounting on a tab switch.
  const jobs = useJobs((s) => s.jobs);
  const setSelectedModel = useCortexStore((s) => s.setSelectedModel);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);

  const reload = useCallback(async () => {
    try {
      const v = await fetchRecommendations();
      setView(v);
      setError(null);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // A pull finishing (ours or one adopted from before a reload) flips a row to
  // "installed" — refresh on the backend's own completion signal so the list
  // is correct even when the pull was started elsewhere.
  useEffect(() => {
    const sub = listen("models:changed", () => void reload());
    return () => {
      void sub.then((un) => un());
    };
  }, [reload]);

  // Fire-and-forget: the job store owns the pull lifecycle (progress events,
  // completion toast + notification), so switching tabs mid-pull loses nothing.
  const onPull = useCallback((rec: ModelRec) => {
    void startCookbookPull(rec.name);
  }, []);

  // "Use in chat" hand-off: select the served model for the composer (the
  // backend routes `ollama:<tag>` through the registered Ollama adapter),
  // close the activity panel so the chat is front-and-center, and focus the
  // composer so the next keystroke starts the conversation.
  const onUse = useCallback(
    (rec: ModelRec) => {
      setSelectedModel(`ollama:${rec.name}`);
      setActivityTab(null);
      window.dispatchEvent(new CustomEvent("cortex:composer-focus"));
      pushToast({ title: `${rec.label} selected for chat`, kind: "success" });
    },
    [setSelectedModel, setActivityTab],
  );

  const specs = view?.specs;
  const canPull = !!specs?.ollama_running;
  const recs = view?.recommendations ?? [];

  if (loading && !view) return <PanelLoading />;

  if (error && !view) {
    return (
      <div className="cookbook-panel">
        <div className="cookbook-error">
          {error}
          <button className="link-btn" onClick={() => void reload()}>Retry</button>
        </div>
      </div>
    );
  }

  return (
    <div className="cookbook-panel">
      {specs && (
        <div className="cookbook-specs">
          <div className="cookbook-spec">
            <Cpu size={15} strokeWidth={1.75} aria-hidden="true" />
            <span className="cookbook-spec-val">{specs.cpu_cores}</span>
            <span className="cookbook-spec-label">cores</span>
          </div>
          <div className="cookbook-spec">
            <HardDrive size={15} strokeWidth={1.75} aria-hidden="true" />
            <span className="cookbook-spec-val">{gb(specs.ram_avail_mb)}</span>
            <span className="cookbook-spec-label">free / {gb(specs.ram_total_mb)}</span>
          </div>
          <div className="cookbook-spec">
            <MonitorCog size={15} strokeWidth={1.75} aria-hidden="true" />
            <span className="cookbook-spec-val">
              {specs.gpu_name ?? "no GPU"}
            </span>
            <span className="cookbook-spec-label">
              {specs.vram_total_mb ? gb(specs.vram_total_mb) : specs.has_cuda ? "CUDA" : "CPU only"}
            </span>
          </div>
        </div>
      )}

      {specs && !specs.ollama_running && (
        <div className="cookbook-callout">
          {specs.ollama_installed
            ? "Ollama is installed but not responding. Start it (`ollama serve`) to pull and serve models."
            : "Ollama isn't installed. Install it from ollama.com, then this Cookbook can pull and serve any model below locally."}
        </div>
      )}

      <ul className="cookbook-list">
        {recs.map((rec) => {
          const job = jobs[`pull:${rec.name}`];
          return (
            <li
              key={rec.name}
              className={`cookbook-row ${rec.fits ? "" : "cookbook-row-unfit"}`}
            >
              <div className="cookbook-row-main">
                <span className="cookbook-name">{rec.label}</span>
                <span className={`cookbook-tier tier-${rec.tier}`}>{rec.tier}</span>
                {rec.installed && (
                  <span className="cookbook-installed">
                    <Check size={12} strokeWidth={2.25} aria-hidden="true" /> installed
                  </span>
                )}
              </div>
              <div className="cookbook-row-meta">
                <span className="cookbook-tag">{rec.name}</span>
                <span className="cookbook-size">{rec.download_gb.toFixed(1)} GB download</span>
                <span className={rec.fits ? "cookbook-fit" : "cookbook-unfit"}>
                  {rec.fit_reason}
                </span>
              </div>
              {job ? (
                <div className="cookbook-progress" role="status">
                  <div className="cookbook-progress-bar">
                    <div
                      className="cookbook-progress-fill"
                      style={{ width: `${Math.max(2, job.pct ?? 0).toFixed(0)}%` }}
                    />
                  </div>
                  <span className="cookbook-progress-label">
                    {job.detail}{job.pct != null ? ` ${job.pct.toFixed(0)}%` : ""}
                  </span>
                </div>
              ) : rec.installed ? (
                <button
                  className="cookbook-use-btn"
                  disabled={!canPull}
                  title={canPull ? `Chat with ${rec.name}` : "Ollama isn't running"}
                  onClick={() => onUse(rec)}
                >
                  <MessageSquare size={13} strokeWidth={1.9} aria-hidden="true" />
                  Use in chat
                </button>
              ) : (
                <button
                  className="cookbook-pull-btn"
                  disabled={!canPull}
                  title={canPull ? `Pull ${rec.name} into Ollama` : "Ollama isn't running"}
                  onClick={() => onPull(rec)}
                >
                  <Download size={13} strokeWidth={1.9} aria-hidden="true" />
                  Pull
                </button>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
