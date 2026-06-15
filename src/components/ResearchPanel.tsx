/**
 * Deep Research panel.
 *
 * Enter a question → the backend plans search queries, runs a keyless web
 * search, fetches the top sources, synthesizes a cited markdown report, and
 * saves it into the vault's `research/` dir. Past reports are listed and
 * re-openable.
 *
 * The run itself lives in the GLOBAL JOB STORE (`state/jobs.ts`,
 * `startDeepResearch`) — this panel only renders the store, so switching
 * tabs mid-run loses neither the progress bar nor the finished report, and
 * `/research <question>` can start a run before this panel ever mounts.
 *
 * A viewed report is not a dead end: the action row opens it in the editor,
 * attaches it to the chat composer as @-context, bookmarks it, or copies the
 * markdown. Bindings live in `src/lib/deep-research.ts`.
 */

import { useCallback, useEffect, useState } from "react";
import {
  Telescope,
  FileText,
  ArrowLeft,
  SquarePen,
  MessageSquarePlus,
  BookmarkPlus,
  Copy,
  CloudOff,
  Settings,
} from "lucide-react";
import { MarkdownView } from "./MarkdownView";
import { humanizeError } from "@/lib/errors";
import { openInEditor } from "@/lib/editor";
import { addBookmark } from "@/lib/bookmarks";
import { pushToast } from "@/lib/toast";
import { useGatewayConfigured } from "@/lib/gateway";
import { useCortexStore } from "@/state/store";
import { useJobs, startDeepResearch, clearResearchReport, type ResearchView } from "@/state/jobs";
import {
  listResearchReports,
  readResearchReport,
  type SavedReport,
} from "@/lib/deep-research";
import "../styles/research.css";

export function ResearchPanel() {
  const [question, setQuestion] = useState("");
  const [maxSources, setMaxSources] = useState(5);
  const [openError, setOpenError] = useState<string | null>(null);
  const [viewedSaved, setViewedSaved] = useState<ResearchView | null>(null);
  const [saved, setSaved] = useState<SavedReport[]>([]);
  const research = useJobs((s) => s.research);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const setShowSettings = useCortexStore((s) => s.setShowSettings);

  // Deep research synthesizes its report with an LLM served by the gateway. With no gateway configured (standalone build) a run can't produce
  // a report, so we degrade the composer to a humanized notice instead of
  // letting the user kick off a run that fails. `null` while the check loads.
  const gateway = useGatewayConfigured();
  const gatewayMissing = gateway === false;

  const running = research.progress !== null;
  // A freshly opened saved report wins over the last run's report; the Back
  // button clears both.
  const report = viewedSaved ?? research.report;
  const error = openError ?? research.error;

  const reloadSaved = useCallback(async () => {
    try {
      setSaved(await listResearchReports());
    } catch {
      /* listing is best-effort */
    }
  }, []);

  // Reload on mount AND whenever a run settles (running flips false) — a run
  // that finished while another tab was open saved its report to the vault.
  useEffect(() => {
    void reloadSaved();
  }, [reloadSaved, running]);

  // A run started elsewhere (`/research`, or before a remount): show its
  // question in the composer so the progress bar has visible context.
  useEffect(() => {
    if (running && research.question) setQuestion(research.question);
  }, [running, research.question]);

  const run = useCallback(() => {
    const q = question.trim();
    if (!q || running || gatewayMissing) return;
    setOpenError(null);
    setViewedSaved(null);
    void startDeepResearch(q, maxSources);
  }, [question, maxSources, running, gatewayMissing]);

  const backToList = useCallback(() => {
    setViewedSaved(null);
    clearResearchReport();
  }, []);

  const openSaved = useCallback(async (r: SavedReport) => {
    try {
      setOpenError(null);
      setViewedSaved({ markdown: await readResearchReport(r.path), path: r.path, title: r.title });
    } catch (e) {
      setOpenError(humanizeError(e));
    }
  }, []);

  const discussInChat = useCallback(() => {
    if (!report?.path) return;
    // Same hand-off shape as Cookbook's "Use in chat": stage the context,
    // close the activity panel so the chat is front-and-center, focus the
    // composer so the next keystroke starts the conversation.
    window.dispatchEvent(
      new CustomEvent("cortex:composer-insert", { detail: { value: `@${report.path} ` } }),
    );
    setActivityTab(null);
    window.dispatchEvent(new CustomEvent("cortex:composer-focus"));
    pushToast({ title: "Report attached to chat", body: report.title, kind: "success" });
  }, [report, setActivityTab]);

  const bookmark = useCallback(async () => {
    if (!report?.path) return;
    const added = await addBookmark({
      kind: "file",
      label: report.title,
      target: report.path,
      tags: ["research"],
      note: null,
    });
    if (added) {
      pushToast({ title: "Report bookmarked", body: report.title, kind: "success" });
    } else {
      pushToast({ title: "Couldn't bookmark report", kind: "error" });
    }
  }, [report]);

  const copyMarkdown = useCallback(async () => {
    if (!report) return;
    try {
      await navigator.clipboard.writeText(report.markdown);
      pushToast({ title: "Report copied as markdown", kind: "success" });
    } catch {
      pushToast({ title: "Copy failed", kind: "error" });
    }
  }, [report]);

  const progress = research.progress;

  return (
    <div className="research-panel">
      {gatewayMissing && (
        <div className="research-gateway-notice" role="status">
          <CloudOff size={16} strokeWidth={1.9} aria-hidden="true" />
          <div className="research-gateway-copy">
            <strong>Deep research needs a gateway</strong>
            <span>
              Reports are synthesized by an LLM served through the Cortex
              Gateway. Connect one to run new research — saved reports below stay
              readable offline.
            </span>
          </div>
          <button
            className="research-gateway-btn"
            onClick={() => setShowSettings(true)}
          >
            <Settings size={13} strokeWidth={1.9} aria-hidden="true" />
            Open Settings
          </button>
        </div>
      )}
      <div className="research-head">
        <textarea
          className="research-input"
          placeholder={
            gatewayMissing
              ? "Connect a gateway to run deep research…"
              : "Ask a research question — e.g. “What changed in HTTP/3 adoption in 2025?”"
          }
          value={question}
          rows={2}
          disabled={running || gatewayMissing}
          onChange={(e) => setQuestion(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) run();
          }}
        />
        <div className="research-controls">
          <label className="research-sources">
            sources
            <select
              value={maxSources}
              disabled={running || gatewayMissing}
              onChange={(e) => setMaxSources(Number(e.target.value))}
            >
              {[3, 5, 7, 10].map((n) => (
                <option key={n} value={n}>{n}</option>
              ))}
            </select>
          </label>
          <button
            className="research-run-btn"
            disabled={running || gatewayMissing || !question.trim()}
            onClick={run}
          >
            <Telescope size={14} strokeWidth={1.9} aria-hidden="true" />
            {running ? "Researching…" : "Research"}
          </button>
        </div>
      </div>

      {progress && (
        <div className="research-progress" role="status">
          <div className="research-progress-bar">
            <div className="research-progress-fill" style={{ width: `${Math.max(3, progress.pct)}%` }} />
          </div>
          <span className="research-progress-label">
            {progress.step}{progress.message ? ` — ${progress.message}` : ""} ({progress.pct}%)
          </span>
        </div>
      )}

      {error && <div className="research-error">{error}</div>}

      <div className="research-body">
        {report ? (
          <div className="research-report">
            <div className="research-actions">
              <button
                className="research-action-btn"
                onClick={backToList}
                title="Back to saved reports"
              >
                <ArrowLeft size={13} strokeWidth={1.9} aria-hidden="true" />
                Reports
              </button>
              <span className="research-actions-spacer" />
              <button
                className="research-action-btn"
                disabled={!report.path}
                title={report.path ?? "Report wasn't saved to the vault"}
                onClick={() => report.path && openInEditor(report.path)}
              >
                <SquarePen size={13} strokeWidth={1.9} aria-hidden="true" />
                Open in editor
              </button>
              <button
                className="research-action-btn"
                disabled={!report.path}
                title={report.path ? "Attach the report to the chat composer" : "Report wasn't saved to the vault"}
                onClick={discussInChat}
              >
                <MessageSquarePlus size={13} strokeWidth={1.9} aria-hidden="true" />
                Discuss in chat
              </button>
              <button
                className="research-action-btn"
                disabled={!report.path}
                title={report.path ? "Bookmark this report" : "Report wasn't saved to the vault"}
                onClick={() => void bookmark()}
              >
                <BookmarkPlus size={13} strokeWidth={1.9} aria-hidden="true" />
                Bookmark
              </button>
              <button
                className="research-action-btn"
                title="Copy the report markdown"
                onClick={() => void copyMarkdown()}
              >
                <Copy size={13} strokeWidth={1.9} aria-hidden="true" />
                Copy
              </button>
            </div>
            <MarkdownView source={report.markdown} />
          </div>
        ) : (
          !running && (
            <div className="research-saved">
              {saved.length === 0 ? (
                <p className="research-hint">No saved reports yet. Ask a question above to run your first deep-research report.</p>
              ) : (
                <>
                  <h3 className="research-saved-title">Saved reports</h3>
                  <ul className="research-saved-list">
                    {saved.map((r) => (
                      <li key={r.path}>
                        <button className="research-saved-row" onClick={() => void openSaved(r)}>
                          <FileText size={14} strokeWidth={1.75} aria-hidden="true" />
                          <span className="research-saved-q">{r.title}</span>
                        </button>
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </div>
          )
        )}
      </div>
    </div>
  );
}
