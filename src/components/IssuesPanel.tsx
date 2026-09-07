import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/backup";
import { useCortexStore } from "@/state/store";
import {
  importIssues,
  openIssuePr,
  previewIssuePr,
  runIssueInLane,
  toIssueRef,
  type ForgeIssue,
  type IssueKind,
  type IssuePrDraft,
  type IssuePrResult,
  type IssueRef,
} from "@/lib/issues";
import { listLaneRuns, listProviders, type LaneRunRecord } from "@/lib/multi-provider";

/** Triage-list badge copy for the auto-classification heuristic (007 full
 *  scope) — a hint, so it reads that way rather than as a verdict. */
const ISSUE_KIND_LABEL: Record<IssueKind, string> = {
  bug: "Bug?",
  feature: "Feature?",
  chore: "Chore?",
  unknown: "",
};

/**
 * Issue-to-Agent pipeline (007, MVP).
 *
 * Import a repo's open issues (read-only; GitHub or GitLab, token from the
 * KeyVault only), triage them, and hand one to an agent on the EXISTING
 * worktree-isolated lane machinery. When the lane settles, "Preview PR" shows
 * the exact draft — nothing is pushed or posted until the human explicitly
 * approves it (a one-shot, expiring approval token backs the gate).
 */
export function IssuesPanel() {
  // ── import form ───────────────────────────────────────────────────────────
  const [forge, setForge] = useState<string>("github");
  const [source, setSource] = useState<string>(() => {
    try {
      return localStorage.getItem("cortex.issues.source") || "";
    } catch {
      return "";
    }
  });
  const [baseUrl, setBaseUrl] = useState("");
  const [issues, setIssues] = useState<ForgeIssue[] | null>(null);
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);

  // ── lane dispatch ─────────────────────────────────────────────────────────
  const [providers, setProviders] = useState<string[]>([]);
  const [providersLoading, setProvidersLoading] = useState(true);
  const [provider, setProvider] = useState<string>("");
  const [giteaProject, setGiteaProject] = useState<string>(() => {
    try {
      return localStorage.getItem("cortex.issues.project") || "";
    } catch {
      return "";
    }
  });
  /** Issue number currently showing the dispatch form. */
  const [armed, setArmed] = useState<number | null>(null);
  const [dispatching, setDispatching] = useState(false);
  const [dispatchError, setDispatchError] = useState<string | null>(null);

  /** Lanes dispatched from THIS panel this session: run_id → the issue. */
  const [issueLanes, setIssueLanes] = useState<{ runId: string; issue: IssueRef }[]>([]);
  const [laneRows, setLaneRows] = useState<Map<string, LaneRunRecord>>(new Map());

  // ── approval-gated PR ─────────────────────────────────────────────────────
  const [previewing, setPreviewing] = useState<string | null>(null);
  const [draft, setDraft] = useState<IssuePrDraft | null>(null);
  const [approving, setApproving] = useState(false);
  const [prResult, setPrResult] = useState<IssuePrResult | null>(null);
  const [prError, setPrError] = useState<string | null>(null);

  const mounted = useRef(true);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const setReplayFocusSpanId = useCortexStore((s) => s.setReplayFocusSpanId);

  /** Jump to this run's local Run Replay recording (007 full scope) — the
   *  same recording the templated PR body points reviewers at. */
  function openRunReplay(runId: string) {
    setReplayFocusSpanId(runId);
    setActivityTab("observability");
  }

  const refreshLanes = useCallback(async () => {
    try {
      const rows = await listLaneRuns();
      if (!mounted.current) return;
      setLaneRows(new Map(rows.map((r) => [r.run_id, r])));
    } catch {
      /* keep the last good rows */
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    listProviders()
      .then((p) => {
        if (!mounted.current) return;
        setProviders(p);
        if (p.length > 0) setProvider((cur) => cur || p[0]);
      })
      .catch(() => {
        /* provider list is optional until dispatch */
      })
      .finally(() => {
        if (mounted.current) setProvidersLoading(false);
      });
    void refreshLanes();
    const unlisten = listen("lanes:updated", () => void refreshLanes());
    return () => {
      mounted.current = false;
      void unlisten.then((u) => u());
    };
  }, [refreshLanes]);

  function parseSlug(value: string): { owner: string; repo: string } | null {
    const m = value.trim().match(/^([\w.-]+)\/([\w.-]+?)(?:\.git)?$/);
    return m ? { owner: m[1], repo: m[2] } : null;
  }

  async function doImport() {
    setImportError(null);
    const src = parseSlug(source);
    if (!src) {
      setImportError("Enter the repo as owner/repo (e.g. octocat/hello-world).");
      return;
    }
    try {
      localStorage.setItem("cortex.issues.source", source.trim());
    } catch {
      /* ignore */
    }
    setImporting(true);
    try {
      const list = await importIssues(forge, src.owner, src.repo, baseUrl || undefined);
      if (mounted.current) setIssues(list);
    } catch (e) {
      if (mounted.current) setImportError(humanizeError(e));
    } finally {
      if (mounted.current) setImporting(false);
    }
  }

  async function dispatchIssue(issue: ForgeIssue) {
    setDispatchError(null);
    const proj = parseSlug(giteaProject);
    if (!proj) {
      setDispatchError("Enter the Gitea project as owner/repo.");
      return;
    }
    if (!provider) {
      setDispatchError("Pick a provider to run the issue with.");
      return;
    }
    try {
      localStorage.setItem("cortex.issues.project", giteaProject.trim());
    } catch {
      /* ignore */
    }
    setDispatching(true);
    try {
      const ref = toIssueRef(issue, baseUrl || undefined);
      const record = await runIssueInLane(proj.owner, proj.repo, provider, ref);
      if (mounted.current) {
        setIssueLanes((prev) => [{ runId: record.run_id, issue: ref }, ...prev]);
        setArmed(null);
      }
      await refreshLanes();
    } catch (e) {
      if (mounted.current) setDispatchError(humanizeError(e));
    } finally {
      if (mounted.current) setDispatching(false);
    }
  }

  async function openPreview(runId: string, issue: IssueRef) {
    setPrError(null);
    setPrResult(null);
    setPreviewing(runId);
    try {
      const d = await previewIssuePr(runId, issue);
      if (mounted.current) setDraft(d);
    } catch (e) {
      if (mounted.current) setPrError(humanizeError(e));
    } finally {
      if (mounted.current) setPreviewing(null);
    }
  }

  async function approve() {
    if (!draft) return;
    setPrError(null);
    setApproving(true);
    try {
      const result = await openIssuePr(draft.approval_token);
      if (mounted.current) setPrResult(result);
    } catch (e) {
      if (mounted.current) setPrError(humanizeError(e));
    } finally {
      if (mounted.current) setApproving(false);
    }
  }

  function closeModal() {
    setDraft(null);
    setPrResult(null);
    setPrError(null);
  }

  function issueAge(iso: string): string {
    const ms = Date.parse(iso);
    return Number.isFinite(ms) ? timeAgo(ms) : "";
  }

  return (
    <div className="lanes-pane">
      <div>
        <h2 className="lanes-title">Issue pipeline</h2>
        <p className="lanes-subtitle">
          Import issues from GitHub or GitLab (read-only), hand one to an agent
          in an isolated lane, then approve the PR by hand — nothing is pushed
          without your explicit approval. Forge tokens are read from the Key
          Vault (provider <code>github</code> / <code>gitlab</code>) and never
          leave the app.
        </p>
      </div>

      <div className="lanes-actions">
        <select
          className="lanes-input"
          value={forge}
          onChange={(e) => setForge(e.target.value)}
          aria-label="Forge"
          style={{ width: "auto" }}
        >
          <option value="github">GitHub</option>
          <option value="gitlab">GitLab</option>
        </select>
        <input
          className="lanes-input"
          value={source}
          onChange={(e) => setSource(e.target.value)}
          placeholder="owner/repo"
          aria-label="Source repo"
          style={{ flex: 1 }}
        />
        <button onClick={() => void doImport()} disabled={importing} className="btn-primary">
          {importing ? "Importing…" : "Import issues"}
        </button>
      </div>
      <label className="lanes-label">
        API base (self-hosted only — leave empty for github.com / gitlab.com)
        <input
          className="lanes-input"
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          placeholder="https://gitlab.example.com"
        />
      </label>
      {importError && <div className="lanes-error">{importError}</div>}

      <div className="lanes-list">
        <div className="lanes-label lanes-label-row">Triage</div>
        {issues === null && !importing && (
          <p className="lanes-hint">
            No issues imported yet — pick a forge and a repo above. Import is
            read-only: it never writes to the tracker.
          </p>
        )}
        {importing && issues === null && <p className="lanes-hint">Importing issues…</p>}
        {issues !== null && issues.length === 0 && (
          <p className="lanes-hint">No open issues in that repo. Nice.</p>
        )}
        {issues?.map((issue) => (
          <div key={`${issue.forge}-${issue.owner}-${issue.repo}-${issue.number}`} className="lanes-card">
            <div className="lanes-card-head">
              <strong className="lanes-card-provider">#{issue.number}</strong>
              <span className="lanes-card-meta">
                {issue.author ? `${issue.author} · ` : ""}
                {issueAge(issue.updated_at)}
              </span>
              <span className={`status-pill lanes-status-${issue.state === "open" || issue.state === "opened" ? "running" : "done"}`}>
                {issue.state}
              </span>
              {ISSUE_KIND_LABEL[issue.kind] && (
                <span
                  className="status-pill"
                  title="Auto-classified from labels/keywords — a triage hint, not a verdict."
                >
                  {ISSUE_KIND_LABEL[issue.kind]}
                </span>
              )}
            </div>
            <div className="lanes-card-task">{issue.title}</div>
            {issue.labels.length > 0 && (
              <div className="lanes-card-meta">{issue.labels.join(" · ")}</div>
            )}
            <div className="lanes-card-actions">
              <a className="btn-ghost lanes-card-btn" href={issue.url} target="_blank" rel="noreferrer">
                Open on {issue.forge} ↗
              </a>
              <button
                className="btn-ghost lanes-card-btn"
                onClick={() => {
                  setDispatchError(null);
                  setArmed(armed === issue.number ? null : issue.number);
                }}
              >
                {armed === issue.number ? "Cancel" : "Run in a lane"}
              </button>
            </div>
            {armed === issue.number && (
              <div className="lanes-card-detail">
                <label className="lanes-label">
                  Gitea project the agent works on (<code>owner/repo</code>)
                  <input
                    className="lanes-input"
                    value={giteaProject}
                    onChange={(e) => setGiteaProject(e.target.value)}
                    placeholder="owner/repo"
                  />
                </label>
                <div className="lanes-label lanes-label-row">Provider</div>
                <div className="lanes-chip-row">
                  {providersLoading && <span className="lanes-hint">Loading providers…</span>}
                  {!providersLoading && providers.length === 0 && (
                    <span className="lanes-hint">
                      No providers available — connect a Cortex Gateway to run issue lanes.
                    </span>
                  )}
                  {providers.map((p) => (
                    <button
                      key={p}
                      onClick={() => setProvider(p)}
                      className={provider === p ? "lanes-chip lanes-chip-on" : "lanes-chip"}
                    >
                      {p}
                    </button>
                  ))}
                </div>
                <div className="lanes-actions">
                  <button
                    className="btn-primary"
                    onClick={() => void dispatchIssue(issue)}
                    disabled={dispatching || !provider}
                  >
                    {dispatching ? "Dispatching…" : `Run issue #${issue.number} in a lane`}
                  </button>
                </div>
                {dispatchError && <div className="lanes-error">{dispatchError}</div>}
              </div>
            )}
          </div>
        ))}
      </div>

      <div className="lanes-list">
        <div className="lanes-label lanes-label-row">Issue lanes (this session)</div>
        {issueLanes.length === 0 && (
          <p className="lanes-hint">
            Lanes dispatched from an issue land here (and in the Lanes tab).
            When one settles, preview and approve its PR — the approval is the
            only thing that ever pushes.
          </p>
        )}
        {issueLanes.map(({ runId, issue }) => {
          const row = laneRows.get(runId);
          const settled = row != null && row.status !== "running" && row.branch != null;
          return (
            <div key={runId} className="lanes-card">
              <div className="lanes-card-head">
                <strong className="lanes-card-provider">{row?.provider ?? "lane"}</strong>
                <span className="lanes-card-meta">
                  issue #{issue.number} · {issue.owner}/{issue.repo}
                </span>
                <span className={`status-pill lanes-status-${row?.status ?? "running"}`}>
                  {row?.status ?? "…"}
                </span>
              </div>
              <div className="lanes-card-task">{issue.title}</div>
              {row?.branch && <code className="lanes-card-branch">branch {row.branch}</code>}
              {row?.detail && (
                <div className={row.status === "error" ? "lanes-card-detail lanes-error" : "lanes-card-detail"}>
                  {row.detail}
                </div>
              )}
              <div className="lanes-card-actions">
                <button
                  className="btn-ghost lanes-card-btn"
                  onClick={() => void openPreview(runId, issue)}
                  disabled={!settled || previewing === runId}
                  title={
                    settled
                      ? "Dry-run the PR — nothing is pushed until you approve"
                      : "Wait for the lane to settle first"
                  }
                >
                  {previewing === runId ? "Previewing…" : "Preview PR"}
                </button>
                <button
                  className="btn-ghost lanes-card-btn"
                  onClick={() => openRunReplay(runId)}
                  title="Open this run's recorded narration/tool-call timeline in Observability → Run Replay"
                >
                  Run Replay ↗
                </button>
              </div>
            </div>
          );
        })}
        {prError && !draft && <div className="lanes-error">{prError}</div>}
      </div>

      {draft && (
        <div className="modal-backdrop" onClick={closeModal}>
          <div className="modal lanes-review-modal" onClick={(e) => e.stopPropagation()}>
            <h2>{draft.title}</h2>
            <div className="lanes-review-meta">
              <code className="lanes-card-branch">head {draft.head_branch}</code>
            </div>
            {prResult ? (
              <div className="lanes-review-note lanes-review-note-ok">
                PR{" "}
                <a href={prResult.pr_url} target="_blank" rel="noreferrer">
                  #{prResult.pr_number} ↗
                </a>{" "}
                opened into <code>{prResult.base}</code>.{" "}
                {prResult.comment_posted
                  ? "Progress comment posted on the issue."
                  : `The PR opened, but the issue comment failed: ${prResult.comment_error ?? "unknown error"}`}
              </div>
            ) : (
              <>
                <pre className="lanes-diff">{draft.body}</pre>
                <p className="lanes-hint">
                  Dry run — nothing has been pushed or posted. Approving opens
                  the review PR on Gitea and comments on the source issue. This
                  approval works once and expires{" "}
                  {timeAgo(draft.expires_unix_ms).replace(" ago", "")} from now.
                </p>
              </>
            )}
            {prError && <div className="lanes-error">{prError}</div>}
            <div className="lanes-review-actions">
              <button className="btn-ghost" onClick={closeModal}>
                {prResult ? "Close" : "Cancel (no writes)"}
              </button>
              {!prResult && (
                <button className="btn-primary" onClick={() => void approve()} disabled={approving}>
                  {approving ? "Opening PR…" : "Approve & open PR"}
                </button>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
