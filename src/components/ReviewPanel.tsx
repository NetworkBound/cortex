import { useCallback, useEffect, useMemo, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { humanizeError } from "@/lib/errors";
import { openInEditor } from "@/lib/editor";
import { join } from "@/lib/path";
import {
  REVIEW_SEVERITIES,
  reviewDiff,
  type ReviewFinding,
  type ReviewReport,
} from "@/lib/review";
import { pushToast } from "@/lib/toast";

/**
 * `/review` results panel. Self-mounting portal (same detached-root pattern
 * as `ExplainModal`) so the slash command can summon it without touching
 * App.tsx. Reuses the `explain-*` shell classes for the backdrop/header and
 * keeps the finding list styling inline so no global CSS changes are needed.
 *
 * On mount we run `review_diff` once; "Re-run" repeats it (useful after
 * fixing the critical findings). Clicking a `file:line` opens the file in the
 * editor pane via `openInEditor`.
 */

interface ReviewPanelProps {
  projectRoot: string;
  base: string | null;
  model: string | null;
  authorModel: string | null;
  onClose: () => void;
}

const SEVERITY_COLOR: Record<string, string> = {
  critical: "var(--danger, #e5484d)",
  high: "var(--warning, #f5a524)",
  medium: "var(--accent, #6e9cff)",
  low: "var(--text-dim)",
  info: "var(--text-dim)",
};

function severityColor(sev: string): string {
  return SEVERITY_COLOR[sev] ?? SEVERITY_COLOR.info;
}

/** Group findings by severity, strongest first; unknown keys land in `info`. */
export function groupBySeverity(
  findings: ReviewFinding[],
): { severity: string; items: ReviewFinding[] }[] {
  const buckets = new Map<string, ReviewFinding[]>();
  for (const s of REVIEW_SEVERITIES) buckets.set(s, []);
  for (const f of findings) {
    const key = buckets.has(f.severity) ? f.severity : "info";
    buckets.get(key)!.push(f);
  }
  return REVIEW_SEVERITIES.filter((s) => buckets.get(s)!.length > 0).map(
    (s) => ({ severity: s, items: buckets.get(s)! }),
  );
}

/**
 * Absolute path for a repo-relative diff path. Git prints `/`; when the
 * project root uses backslashes (Windows) we match its flavour so the editor
 * pane and store compare paths consistently.
 */
export function absolutePathFor(projectRoot: string, file: string): string {
  const joined = join(projectRoot, file);
  return projectRoot.includes("\\") && !projectRoot.includes("/")
    ? joined.replace(/\//g, "\\")
    : joined;
}

export function ReviewPanel({
  projectRoot,
  base,
  model,
  authorModel,
  onClose,
}: ReviewPanelProps) {
  const [report, setReport] = useState<ReviewReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [runId, setRunId] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    reviewDiff({
      project_root: projectRoot,
      base,
      agent: model,
      author_model: authorModel,
    })
      .then((r) => {
        if (!cancelled) setReport(r);
      })
      .catch((e) => {
        if (!cancelled) setError(humanizeError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectRoot, base, model, authorModel, runId]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const groups = useMemo(
    () => (report ? groupBySeverity(report.findings) : []),
    [report],
  );

  const open = useCallback(
    (f: ReviewFinding) => {
      if (!f.file) return;
      openInEditor(absolutePathFor(projectRoot, f.file));
      if (f.line != null) {
        pushToast({
          title: `${f.file}:${f.line}`,
          body: f.title,
          kind: "info",
        });
      }
    },
    [projectRoot],
  );

  const copy = useCallback(async () => {
    if (!report) return;
    const lines: string[] = [];
    if (report.summary) lines.push(report.summary, "");
    for (const f of report.findings) {
      const loc = f.file
        ? ` — ${f.file}${f.line != null ? `:${f.line}` : ""}`
        : "";
      lines.push(`[${f.severity}] ${f.title}${loc}`);
      if (f.detail) lines.push(`  ${f.detail}`);
      if (f.suggestion) lines.push(`  Suggestion: ${f.suggestion}`);
    }
    try {
      await navigator.clipboard.writeText(lines.join("\n"));
      pushToast({ title: "Review copied", kind: "success" });
    } catch (e) {
      pushToast({
        title: "Copy failed",
        body: humanizeError(e),
        kind: "error",
      });
    }
  }, [report]);

  const scope = base ? `vs ${base}` : "uncommitted changes";

  return (
    <div className="explain-backdrop" onClick={onClose}>
      <div
        className="explain-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="review-title"
        style={{ width: "min(900px, 100%)" }}
      >
        <header className="explain-header">
          <div>
            <h2 id="review-title">Code review</h2>
            <div className="explain-path" title={projectRoot}>
              {scope} · {projectRoot}
            </div>
          </div>
          <div style={{ display: "flex", gap: "var(--space-2)" }}>
            <button
              className="explain-close"
              onClick={() => setRunId((n) => n + 1)}
              disabled={loading}
              title="Run the review again"
              style={{ fontSize: "var(--text-sm)" }}
            >
              Re-run
            </button>
            <button
              className="explain-close"
              onClick={copy}
              disabled={!report}
              title="Copy findings as text"
              style={{ fontSize: "var(--text-sm)" }}
            >
              Copy
            </button>
            <button
              className="explain-close"
              onClick={onClose}
              aria-label="Close"
            >
              ×
            </button>
          </div>
        </header>

        <div
          style={{
            padding: "var(--space-4) var(--space-5)",
            overflow: "auto",
            minHeight: 0,
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-3)",
          }}
        >
          {loading && (
            <div style={{ color: "var(--text-dim)" }}>
              Reviewing {scope}
              {model ? ` with ${model}` : ""}…
            </div>
          )}
          {!loading && error && (
            <div style={{ color: "var(--danger, #e5484d)" }}>{error}</div>
          )}
          {!loading && report && (
            <>
              <div
                style={{
                  fontSize: "var(--text-xs)",
                  color: "var(--text-dim)",
                  display: "flex",
                  flexWrap: "wrap",
                  gap: "var(--space-3)",
                }}
              >
                <span title="Reviewer model">
                  {report.model}
                  {report.cross_model ? " (cross-model)" : ""}
                  {report.fell_back ? " (fallback)" : ""}
                </span>
                <span>
                  {report.files.length} file(s) ·{" "}
                  {Math.round(report.diff_bytes / 1024)} KiB
                </span>
                <span>{(report.latency_ms / 1000).toFixed(1)}s</span>
                {report.truncated && (
                  <span title="Large files were cut; findings may be incomplete">
                    diff truncated
                  </span>
                )}
              </div>
              {report.summary && <p style={{ margin: 0 }}>{report.summary}</p>}
              {report.findings.length === 0 && (
                <div style={{ color: "var(--text-dim)" }}>
                  No findings — the reviewer had nothing to flag.
                </div>
              )}
              {groups.map((g) => (
                <section key={g.severity}>
                  <div
                    className="explain-col-label"
                    style={{ color: severityColor(g.severity) }}
                  >
                    {g.severity} · {g.items.length}
                  </div>
                  <ul
                    style={{
                      listStyle: "none",
                      margin: 0,
                      padding: 0,
                      display: "flex",
                      flexDirection: "column",
                      gap: "var(--space-2)",
                    }}
                  >
                    {g.items.map((f, i) => (
                      <li
                        key={`${g.severity}-${i}`}
                        style={{
                          border: "1px solid var(--border)",
                          borderLeft: `3px solid ${severityColor(g.severity)}`,
                          borderRadius: "var(--radius-sm)",
                          padding: "var(--space-2) var(--space-3)",
                          background: "var(--bg-sunken)",
                        }}
                      >
                        <div
                          style={{
                            display: "flex",
                            justifyContent: "space-between",
                            gap: "var(--space-3)",
                            alignItems: "baseline",
                          }}
                        >
                          <strong>{f.title}</strong>
                          {f.file && (
                            <button
                              onClick={() => open(f)}
                              title="Open in editor"
                              style={{
                                background: "transparent",
                                border: "none",
                                color: "var(--accent, #6e9cff)",
                                cursor: "pointer",
                                fontFamily: "var(--font-mono)",
                                fontSize: "var(--text-xs)",
                                whiteSpace: "nowrap",
                                padding: 0,
                              }}
                            >
                              {f.file}
                              {f.line != null ? `:${f.line}` : ""}
                            </button>
                          )}
                        </div>
                        {f.detail && (
                          <div
                            style={{
                              marginTop: "var(--space-1)",
                              whiteSpace: "pre-wrap",
                              fontSize: "var(--text-sm)",
                            }}
                          >
                            {f.detail}
                          </div>
                        )}
                        {f.suggestion && (
                          <div
                            style={{
                              marginTop: "var(--space-1)",
                              whiteSpace: "pre-wrap",
                              fontSize: "var(--text-sm)",
                              color: "var(--text-dim)",
                            }}
                          >
                            Suggestion: {f.suggestion}
                          </div>
                        )}
                      </li>
                    ))}
                  </ul>
                </section>
              ))}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/review` slash command. Same
 * detached-root pattern as `ExplainModal`.
 */
let activeRoot: Root | null = null;

export function openReviewPanel(
  projectRoot: string,
  base: string | null = null,
  model: string | null = null,
  authorModel: string | null = null,
): void {
  if (activeRoot) return; // already open
  if (!projectRoot) {
    pushToast({
      title: "No project",
      body: "Pick a project from the sidebar first.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "review";
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
    <ReviewPanel
      projectRoot={projectRoot}
      base={base}
      model={model}
      authorModel={authorModel}
      onClose={close}
    />,
  );
}
