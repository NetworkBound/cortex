import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  dailyJournal,
  saveJournal,
  type JournalReport,
} from "@/lib/daily-journal";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Daily journal modal. Renders as a self-mounting portal so the `/journal`
 * slash command can summon it without touching App.tsx — same pattern as
 * `ExplainModal` / `DuckChat`.
 *
 * On mount we call `daily_journal` for the chosen date (defaults to today).
 * The user can swap dates with the picker — the call re-fires and the
 * markdown body re-renders. "Save to Cortex Brain" writes the file out
 * under `~/Documents/Cortex Brain/journal/<date>.md`.
 */

interface DailyJournalModalProps {
  initialDate: string;
  onClose: () => void;
}

function todayYmd(): string {
  const d = new Date();
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

export function DailyJournalModal({
  initialDate,
  onClose,
}: DailyJournalModalProps) {
  const [date, setDate] = useState<string>(initialDate || todayYmd());
  const [report, setReport] = useState<JournalReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [saving, setSaving] = useState<boolean>(false);

  const projectRoot = useCortexStore((s) => s.activeProject?.root) ?? null;

  // Re-fires on date / projectRoot change.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await dailyJournal({ projectRoot, date });
        if (cancelled) return;
        setReport(out);
      } catch (e) {
        if (cancelled) return;
        setError(humanizeError(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [date, projectRoot]);

  // ESC closes — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onSave = useCallback(async () => {
    if (!report || saving) return;
    setSaving(true);
    try {
      const saved = await saveJournal({
        date: report.date,
        markdown: report.markdown,
        stats: report.stats,
      });
      pushToast({
        title: "Saved to Brain",
        body: saved.written_path,
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Save failed", body: humanizeError(e), kind: "error" });
    } finally {
      setSaving(false);
    }
  }, [report, saving]);

  const onCopy = useCallback(async () => {
    if (!report) return;
    try {
      await navigator.clipboard.writeText(report.markdown);
      pushToast({
        title: "Copied",
        body: "Journal markdown on clipboard.",
        kind: "success",
      });
    } catch (e) {
      pushToast({ title: "Copy failed", body: humanizeError(e), kind: "error" });
    }
  }, [report]);

  return (
    <div className="journal-backdrop" onClick={onClose}>
      <div
        className="journal-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="journal-title"
      >
        <header className="journal-header">
          <div className="journal-header-main">
            <h2 id="journal-title">Daily journal</h2>
            <input
              className="journal-date"
              type="date"
              value={date}
              onChange={(e) => setDate(e.target.value || todayYmd())}
              aria-label="Journal date"
              max={todayYmd()}
            />
          </div>
          <button className="journal-close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <div className="journal-stats" role="group" aria-label="Activity counts">
          <Stat label="Sessions" value={report?.stats.sessions} />
          <Stat label="Commits" value={report?.stats.commits} />
          <Stat label="Memory" value={report?.stats.memory_updates} />
          <Stat label="Snapshots" value={report?.stats.snapshots} />
          <Stat label="PRPs" value={report?.stats.prp_advances} />
        </div>

        <div className="journal-body">
          {loading && (
            <div className="journal-loading">
              <span className="journal-spinner" aria-hidden /> Building
              journal…
            </div>
          )}
          {error && !loading && (
            <div className="journal-error" role="alert">
              Failed to build journal.
              <pre>{error}</pre>
            </div>
          )}
          {report && !loading && !error && (
            <div className="journal-markdown">
              <ReactMarkdown remarkPlugins={[remarkGfm]}>
                {report.markdown}
              </ReactMarkdown>
            </div>
          )}
        </div>

        <footer className="journal-footer">
          <button
            className="journal-action"
            onClick={onCopy}
            disabled={!report || loading}
          >
            Copy markdown
          </button>
          <button
            className="journal-action"
            onClick={onSave}
            disabled={!report || loading || saving}
          >
            {saving ? "Saving…" : "Save to Cortex Brain"}
          </button>
          <button className="journal-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: number | undefined }) {
  return (
    <div className="journal-stat">
      <div className="journal-stat-value">{value ?? "–"}</div>
      <div className="journal-stat-label">{label}</div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/journal` slash command. Same detached-
 * root pattern as `ExplainModal` / `DuckChat`.
 */
let activeRoot: Root | null = null;

export function openDailyJournalModal(date?: string): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "journal";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(
    <DailyJournalModal
      initialDate={(date || "").trim() || todayYmd()}
      onClose={close}
    />,
  );
}
