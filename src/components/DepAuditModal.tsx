import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import {
  advisoryUrl,
  auditDeps,
  normalizeSeverity,
  packageRegistryUrl,
  type DepAuditReport,
  type Severity,
  type Vulnerability,
} from "@/lib/dep-audit";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

/**
 * Dependency vulnerability audit modal. Renders as a self-mounting portal
 * so the `/audit-deps` slash command can summon it without App.tsx wiring
 * — same pattern as `RefactorSuggesterModal` / `ConflictResolverModal`.
 *
 * On mount we kick off `audit_deps` against the active project's root. The
 * top of the panel shows severity counts (critical / high / medium / low /
 * unknown); the body is a severity-tinted list of `Vulnerability` rows.
 *
 * Per-row actions:
 *   - "Explain" — posts a chat note asking the gateway to explain the CVE in
 *     plain language. We don't auto-route to `explain_code` because the
 *     ecosystem is package-level, not file-level — there's no source path.
 *   - "Open package" — uses `@tauri-apps/plugin-shell::open` against the
 *     ecosystem-canonical registry URL (npm / crates.io / pypi).
 *   - "View advisory" — opens the CVE / RUSTSEC / GHSA landing page when
 *     an id is present.
 */

interface DepAuditModalProps {
  projectRoot: string;
  onClose: () => void;
}

export function DepAuditModal({ projectRoot, onClose }: DepAuditModalProps) {
  const [report, setReport] = useState<DepAuditReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState<Severity | "all">("all");

  // Kick off the audit on mount. The backend itself handles the "tool not
  // installed" case by throwing a descriptive string we render verbatim.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError(null);
      try {
        const out = await auditDeps(projectRoot);
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
  }, [projectRoot]);

  // ESC closes the modal — standard for transient surfaces.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onExplain = useCallback((v: Vulnerability, ecosystem: string) => {
    // Post a chat note that the user can hand to the live chat input —
    // we don't auto-submit because the user may want to tweak the prompt
    // or pick a different agent. The note doubles as a record of "I
    // looked at this CVE" for the session scrollback.
    const cve = v.cve ?? "(no id)";
    const fix = v.fix_available ?? "(no fix listed)";
    const note = [
      `🛡️ **Explain vulnerability** — \`${v.package}@${v.version || "?"}\` (${ecosystem})`,
      "",
      `**Severity**: ${normalizeSeverity(v.severity)} (${v.severity})`,
      `**Advisory id**: ${cve}`,
      `**Summary**: ${v.summary}`,
      `**Fix available**: ${fix}`,
      "",
      `Ask: in plain language, what does ${cve} mean for our project, and what's the cheapest mitigation?`,
    ].join("\n");
    useCortexStore.getState().appendMessage({
      id: `da-${crypto.randomUUID()}`,
      role: "system",
      content: note,
      tools: [],
    });
    pushToast({
      title: "Posted to chat",
      body: `${v.package} — ask the gateway from the composer.`,
      kind: "success",
    });
  }, []);

  const onOpenPackage = useCallback(
    async (v: Vulnerability, ecosystem: string) => {
      const url = packageRegistryUrl(ecosystem, v.package);
      try {
        await shellOpen(url);
      } catch (e) {
        pushToast({
          title: "Open failed",
          body: humanizeError(e),
          kind: "error",
        });
      }
    },
    [],
  );

  const onOpenAdvisory = useCallback(async (cve: string) => {
    try {
      await shellOpen(advisoryUrl(cve));
    } catch (e) {
      pushToast({
        title: "Open failed",
        body: humanizeError(e),
        kind: "error",
      });
    }
  }, []);

  const filtered =
    !report || filter === "all"
      ? report?.vulnerabilities ?? []
      : report.vulnerabilities.filter(
          (v) => normalizeSeverity(v.severity) === filter,
        );

  return (
    <div className="dep-audit-backdrop" onClick={onClose}>
      <div
        className="dep-audit-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-labelledby="dep-audit-title"
      >
        <header className="dep-audit-header">
          <div>
            <h2 id="dep-audit-title">Dependency vulnerability audit</h2>
            <div className="dep-audit-path" title={projectRoot}>
              {projectRoot}
              {report ? ` · ${report.ecosystem}` : ""}
            </div>
          </div>
          <button
            className="dep-audit-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </header>

        {report && !loading && (
          <div className="dep-audit-summary" role="group" aria-label="Severity counts">
            {(
              [
                ["critical", report.summary.critical],
                ["high", report.summary.high],
                ["medium", report.summary.medium],
                ["low", report.summary.low],
                ["unknown", report.summary.unknown],
              ] as Array<[Severity, number]>
            ).map(([sev, count]) => (
              <button
                key={sev}
                type="button"
                className="dep-audit-summary-pill"
                data-severity={sev}
                data-active={filter === sev}
                onClick={() => setFilter(filter === sev ? "all" : sev)}
                title={`Filter to ${sev}`}
              >
                <span className="dep-audit-summary-count">{count}</span>
                <span className="dep-audit-summary-label">{sev}</span>
              </button>
            ))}
            <button
              type="button"
              className="dep-audit-summary-pill"
              data-severity="all"
              data-active={filter === "all"}
              onClick={() => setFilter("all")}
              title="Show all"
            >
              <span className="dep-audit-summary-count">
                {report.vulnerabilities.length}
              </span>
              <span className="dep-audit-summary-label">all</span>
            </button>
            {report.total_count > report.vulnerabilities.length && (
              <span className="dep-audit-cap-note">
                Showing first {report.vulnerabilities.length} of {report.total_count} (capped).
              </span>
            )}
          </div>
        )}

        <div className="dep-audit-body">
          {loading && (
            <div className="dep-audit-loading">
              <span className="dep-audit-spinner" aria-hidden /> Running audit…
            </div>
          )}
          {error && !loading && (
            <div className="dep-audit-error">
              Audit failed:
              <pre>{error}</pre>
            </div>
          )}
          {!loading && !error && report && report.vulnerabilities.length === 0 && (
            <div className="dep-audit-empty">
              <p>No vulnerabilities found. 🎉</p>
              {report.raw_output_tail && (
                <details>
                  <summary>Tool output tail</summary>
                  <pre>{report.raw_output_tail}</pre>
                </details>
              )}
            </div>
          )}
          {!loading && !error && filtered.length > 0 && report && (
            <ul className="dep-audit-list">
              {filtered.map((v, i) => {
                const sev = normalizeSeverity(v.severity);
                return (
                  <li
                    key={`${v.package}-${v.cve ?? i}`}
                    className="dep-audit-row"
                    data-severity={sev}
                  >
                    <div className="dep-audit-row-head">
                      <span
                        className="dep-audit-row-severity"
                        data-severity={sev}
                      >
                        {sev}
                      </span>
                      <span className="dep-audit-row-pkg">
                        {v.package}
                        {v.version ? (
                          <span className="dep-audit-row-version">@{v.version}</span>
                        ) : null}
                      </span>
                      {v.cve && (
                        <button
                          type="button"
                          className="dep-audit-row-cve"
                          onClick={() => onOpenAdvisory(v.cve!)}
                          title={`Open advisory ${v.cve}`}
                        >
                          {v.cve}
                        </button>
                      )}
                    </div>
                    <div className="dep-audit-row-summary">{v.summary}</div>
                    {v.fix_available && (
                      <div className="dep-audit-row-fix">
                        Fix: <code>{v.fix_available}</code>
                      </div>
                    )}
                    <div className="dep-audit-row-actions">
                      <button
                        type="button"
                        className="dep-audit-action"
                        onClick={() => onExplain(v, report.ecosystem)}
                      >
                        Explain
                      </button>
                      <button
                        type="button"
                        className="dep-audit-action"
                        onClick={() => onOpenPackage(v, report.ecosystem)}
                      >
                        Open package
                      </button>
                      {v.cve && (
                        <button
                          type="button"
                          className="dep-audit-action"
                          onClick={() => onOpenAdvisory(v.cve!)}
                        >
                          View advisory
                        </button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <footer className="dep-audit-footer">
          <button className="dep-audit-primary" onClick={onClose}>
            Close
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/audit-deps` slash command. Same
 * detached-root pattern as `RefactorSuggesterModal` / `ConflictResolverModal`.
 */
let activeRoot: Root | null = null;

export function openDepAuditModal(projectRoot: string): void {
  if (activeRoot) return; // already open
  if (!projectRoot) {
    pushToast({
      title: "No project",
      body: "Pick a project from the sidebar before running /audit-deps.",
      kind: "warning",
    });
    return;
  }
  const container = document.createElement("div");
  container.dataset.cortexMount = "dep-audit";
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
  root.render(<DepAuditModal projectRoot={projectRoot} onClose={close} />);
}
