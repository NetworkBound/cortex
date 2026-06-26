import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";
import { PanelLoading } from "./Skeleton";
import {
  budgetStatus,
  createTeam,
  deleteTeam,
  formatUsd,
  isTeamRunning,
  listTeams,
  runTeam,
  timeAgo,
  truncateTask,
  type Team,
  type Worker,
  type WorkerStatus,
} from "@/lib/teams";
import { listRoles, type Role } from "@/lib/roles";

/**
 * Reveal a recorded run transcript in the chat pane — the same hand-off the
 * Routines history and notification deep-links use.
 */
function openTranscript(sessionId: string) {
  window.dispatchEvent(
    new CustomEvent("cortex:chat-replay", { detail: { session_id: sessionId } }),
  );
  useCortexStore.getState().setActivityTab(null);
}

/**
 * Live dashboard for long-running multi-agent teams (ContextForge #8 — the
 * tmux-orchestrator pattern adapted to a single-process Tauri app).
 *
 * Layout:
 *   ┌─────────────────────────────────────────────────────────────┐
 *   │ Teams                                       [+ New team]    │
 *   │ ┌─────────────┐ ┌─────────────┐ ┌─────────────┐             │
 *   │ │ team chip   │ │ …           │ │ …           │             │
 *   │ └─────────────┘ └─────────────┘ └─────────────┘             │
 *   │ ─────────────────────────────────────────────────           │
 *   │            ┌──────────────────────────┐                     │
 *   │            │ Manager (system-architect) │                   │
 *   │            └──────────────────────────┘                     │
 *   │   ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐        │
 *   │   │ Worker  │  │ Worker  │  │ Worker  │  │ Worker  │        │
 *   │   └─────────┘  └─────────┘  └─────────┘  └─────────┘        │
 *   └─────────────────────────────────────────────────────────────┘
 *
 * Auto-refresh every 5s while mounted. The "+ New team" modal pulls available
 * roles via the existing `list_roles` command so the picker stays in sync with
 * the rest of the role surface (no duplicated registry).
 */
export function OrchestratorView() {
  const [teams, setTeams] = useState<Team[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [assignTeam, setAssignTeam] = useState<Team | null>(null);

  const refresh = useCallback(async () => {
    try {
      const next = await listTeams();
      setTeams(next);
      setError(null);
      setSelectedId((prev) => {
        if (prev && next.some((t) => t.id === prev)) return prev;
        return next[0]?.id ?? null;
      });
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    // 5s auto-refresh — cheap (single JSON read per team file). Stops when the
    // component unmounts so we don't pin a background tick after the user
    // closes the orchestrator tab.
    const id = setInterval(() => {
      void refresh();
    }, 5000);
    // The team runner emits `teams:updated` after every persisted transition
    // (plan landed, worker started/finished) — refresh immediately so live
    // runs read as live instead of up-to-5s stale.
    const unlisten = listen("teams:updated", () => {
      void refresh();
    });
    return () => {
      clearInterval(id);
      void unlisten.then((off) => off());
    };
  }, [refresh]);

  const selected = useMemo(
    () => teams.find((t) => t.id === selectedId) ?? null,
    [teams, selectedId],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      if (!(await confirmDialog({
        title: "Delete team?",
        message: "The team will be deleted. Worker history is not preserved.",
        confirmLabel: "Delete",
        danger: true,
      }))) return;
      try {
        await deleteTeam(id);
        await refresh();
      } catch (e) {
        setError(humanizeError(e));
      }
    },
    [refresh],
  );

  return (
    <div className="orch-root">
      <div className="orch-head">
        <span className="orch-title">Orchestrator</span>
        <button className="orch-new-btn" onClick={() => setShowCreate(true)}>
          + New team
        </button>
      </div>

      <div className="orch-team-strip">
        {loading && teams.length === 0 ? (
          <PanelLoading label="Loading teams" />
        ) : teams.length === 0 && !error ? (
          <div className="muted orch-empty">
            No teams yet. Click <strong>+ New team</strong> to spin one up.
          </div>
        ) : (
          teams.map((t) => (
            <button
              key={t.id}
              className={`orch-team-chip ${t.id === selectedId ? "active" : ""}`}
              onClick={() => setSelectedId(t.id)}
              title={`${t.workers.length} worker(s) · ${timeAgo(t.created_unix_ms)}`}
            >
              <span className="orch-team-chip-name">{t.name}</span>
              <span className="orch-team-chip-meta">
                {t.workers.length} · {timeAgo(t.created_unix_ms)}
              </span>
            </button>
          ))
        )}
      </div>

      {error ? <div className="orch-error">{error}</div> : null}

      {selected ? (
        <TeamGrid
          team={selected}
          onDelete={() => handleDelete(selected.id)}
          onAssign={() => setAssignTeam(selected)}
        />
      ) : !error ? (
        <div className="orch-placeholder muted">Select or create a team to begin.</div>
      ) : null}

      {assignTeam ? (
        <AssignGoalModal
          team={assignTeam}
          onClose={() => setAssignTeam(null)}
          onStarted={async () => {
            setAssignTeam(null);
            await refresh();
          }}
        />
      ) : null}

      {showCreate ? (
        <CreateTeamModal
          existingNames={new Set(teams.map((t) => t.name))}
          onClose={() => setShowCreate(false)}
          onCreated={async (newId) => {
            setShowCreate(false);
            setSelectedId(newId);
            await refresh();
          }}
        />
      ) : null}
    </div>
  );
}

function TeamGrid({
  team,
  onDelete,
  onAssign,
}: {
  team: Team;
  onDelete: () => void;
  onAssign: () => void;
}) {
  const running = isTeamRunning(team);
  return (
    <div className="orch-grid">
      <div className="orch-grid-head">
        <ManagerCard team={team} />
        <div className="orch-head-actions">
          <button
            className="orch-assign-btn"
            onClick={onAssign}
            disabled={running || team.workers.length === 0}
            title={
              team.workers.length === 0
                ? "This team has no workers to assign work to."
                : running
                  ? "A run is already in flight."
                  : "Give the team a goal — the manager plans, the workers execute."
            }
          >
            {running ? "Running…" : "Assign goal"}
          </button>
          <button
            className="orch-delete-btn"
            onClick={onDelete}
            disabled={running}
            title={running ? "Wait for the run to finish first." : "Delete this team"}
          >
            Delete
          </button>
        </div>
      </div>
      <div className="orch-worker-grid">
        {team.workers.length === 0 ? (
          <div className="muted orch-empty">No workers — add some when you create the next team.</div>
        ) : (
          team.workers.map((w) => <WorkerCard key={w.agent_id} worker={w} />)
        )}
      </div>
    </div>
  );
}

/** Map the team-level run lifecycle onto the existing worker pill palette. */
function runPillClass(runStatus: string): string {
  if (runStatus === "planning" || runStatus === "running") return "orch-pill-working";
  if (runStatus === "done") return "orch-pill-done";
  return "orch-pill-error";
}

function ManagerCard({ team }: { team: Team }) {
  return (
    <div className="orch-manager-card">
      <div className="orch-card-row">
        <span className="orch-role-badge">manager</span>
        <strong className="orch-card-title">{team.name}</strong>
        {team.run_status ? (
          <span className={`orch-pill ${runPillClass(team.run_status)}`}>
            {team.run_status}
          </span>
        ) : null}
      </div>
      {team.goal ? (
        <div className="orch-task" title={team.goal}>
          {truncateTask(team.goal, 160)}
        </div>
      ) : (
        <div className="orch-task muted">
          No goal yet — assign one to put this team to work.
        </div>
      )}
      <div className="orch-card-meta">
        <span>{team.manager_role}</span>
        <span>·</span>
        <span>{team.workers.length} worker{team.workers.length === 1 ? "" : "s"}</span>
        <span>·</span>
        <span>created {timeAgo(team.created_unix_ms)}</span>
        {team.last_run_unix_ms ? (
          <>
            <span>·</span>
            <span>last run {timeAgo(team.last_run_unix_ms)}</span>
          </>
        ) : null}
        {team.spent_usd != null ? (
          <>
            <span>·</span>
            <span
              className="orch-cost-badge"
              title="Estimated total spend across this run's workers (token estimate × model price)"
            >
              {formatUsd(team.spent_usd)}
            </span>
          </>
        ) : null}
        {team.plan_session_id ? (
          <button
            className="orch-transcript-btn"
            onClick={() => openTranscript(team.plan_session_id!)}
            title="Open the manager's planning transcript in chat"
          >
            Plan transcript
          </button>
        ) : null}
        {team.synthesis_session_id ? (
          <button
            className="orch-transcript-btn orch-synthesis-btn"
            onClick={() => openTranscript(team.synthesis_session_id!)}
            title="Open the manager's synthesis & verification of all worker outputs in chat"
          >
            Synthesis
          </button>
        ) : null}
      </div>
      <BudgetGauge team={team} />
    </div>
  );
}

/**
 * Projected-vs-budget gauge (orchestration slice 6). The orchestrator's only
 * honest cost signal is the *projected* spend (`spent_usd` — a token-estimate ×
 * the shared pricing table; the one-shot worker calls surface no real per-token
 * usage, so there's no "actual" to chart). When the team carries a soft budget
 * we render that projection against the ceiling with a fill bar and a soft
 * over-budget warning — never a block, matching the backend (a run is never
 * killed for crossing budget).
 */
function BudgetGauge({ team }: { team: Team }) {
  const status = budgetStatus(team.budget_usd, team.spent_usd);
  if (!status) return null;
  const { budget, projected, ratio, over } = status;
  const hasRun = team.spent_usd != null;
  return (
    <div className={`orch-budget ${over ? "is-over" : ""}`}>
      <div className="orch-budget-head">
        <span className="orch-budget-label">Budget</span>
        <span className="orch-budget-figures">
          <span className="orch-budget-projected" title="Projected spend for the latest run (token estimate × model price)">
            {hasRun ? formatUsd(projected) : "—"}
          </span>
          <span className="orch-budget-sep">/</span>
          <span className="orch-budget-cap" title="Soft spend ceiling for this team">
            {formatUsd(budget)}
          </span>
        </span>
      </div>
      <div className="orch-budget-track" role="presentation">
        <div
          className="orch-budget-fill"
          style={{ width: `${Math.round(ratio * 100)}%` }}
        />
      </div>
      {over ? (
        <div className="orch-budget-warn" title="The run's projected spend exceeded the soft budget — runs are never blocked, this is a heads-up only">
          Projected spend exceeds budget by {formatUsd(projected - budget)}.
        </div>
      ) : null}
    </div>
  );
}

function WorkerCard({ worker }: { worker: Worker }) {
  const status = worker.status as WorkerStatus;
  return (
    <div className="orch-worker-card">
      <div className="orch-card-row">
        <span className="orch-role-badge">{worker.role}</span>
        <span className={`orch-pill orch-pill-${status}`}>{worker.status}</span>
      </div>
      <div className="orch-task" title={worker.current_task ?? ""}>
        {truncateTask(worker.current_task)}
      </div>
      {worker.task_kind || worker.task_difficulty ? (
        <div className="orch-worker-tags">
          {worker.task_kind ? (
            <span
              className={`orch-tag orch-tag-kind-${worker.task_kind}`}
              title="Task kind from the manager's plan"
            >
              {worker.task_kind}
            </span>
          ) : null}
          {worker.task_difficulty ? (
            <span
              className={`orch-tag orch-tag-diff-${worker.task_difficulty}`}
              title="Task difficulty from the manager's plan"
            >
              {worker.task_difficulty}
            </span>
          ) : null}
        </div>
      ) : null}
      {worker.effective_model ? (
        <div className="orch-worker-route" title="Model this worker was dispatched on (cost-aware routing)">
          <span className="orch-route-model">{worker.effective_model}</span>
          {worker.projected_usd != null ? (
            <span className="orch-route-cost">{formatUsd(worker.projected_usd)}</span>
          ) : null}
        </div>
      ) : null}
      <div className="orch-card-meta">
        <span>started {timeAgo(worker.started_unix_ms)}</span>
        <span>·</span>
        <span>last {timeAgo(worker.last_event_unix_ms)}</span>
        <span>·</span>
        <span>{worker.message_count} msg{worker.message_count === 1 ? "" : "s"}</span>
        {worker.session_id ? (
          <button
            className="orch-transcript-btn"
            onClick={() => openTranscript(worker.session_id!)}
            title="Open this worker's run transcript in chat"
          >
            Transcript
          </button>
        ) : null}
        {worker.lane_run_id ? (
          <button
            className="orch-transcript-btn"
            onClick={() => useCortexStore.getState().setActivityTab("lanes")}
            title="This code task ran in a gateway worktree — review the diff in the Lanes tab"
          >
            View lane
          </button>
        ) : null}
      </div>
    </div>
  );
}

interface AssignGoalModalProps {
  team: Team;
  onClose: () => void;
  onStarted: () => void | Promise<void>;
}

/**
 * "Assign goal" — the action that turns a team from a roster into a run. The
 * goal goes to the manager for planning; the run continues backend-side, so
 * closing this modal (or the whole tab) never cancels it.
 */
function AssignGoalModal({ team, onClose, onStarted }: AssignGoalModalProps) {
  const [goal, setGoal] = useState(team.goal ?? "");
  // Optional Gitea owner/repo: when set, code-tagged subtasks edit it in a
  // gateway worktree lane (slice 4). Prefilled from the Lanes pane's last repo.
  const [repo, setRepo] = useState<string>(() => {
    try {
      return localStorage.getItem("cortex.lanes.project") || "";
    } catch {
      return "";
    }
  });
  // Optional soft budget (slice 6): prefilled from the team's existing ceiling
  // so re-assigning keeps it. Blank = leave the team's budget unchanged.
  const [budget, setBudget] = useState<string>(
    team.budget_usd != null ? String(team.budget_usd) : "",
  );
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const selectedModel = useCortexStore((s) => s.selectedModel);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const budgetInvalid =
    budget.trim().length > 0 && !(Number(budget) >= 0 && Number.isFinite(Number(budget)));
  const canSubmit = !submitting && goal.trim().length > 0 && !budgetInvalid;

  const handleSubmit = async () => {
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      const repoArg = repo.trim() || null;
      if (repoArg) {
        try {
          localStorage.setItem("cortex.lanes.project", repoArg);
        } catch {
          /* private mode / quota — non-fatal */
        }
      }
      // Blank budget field leaves the team's existing ceiling untouched (null);
      // a number updates it for this and future runs.
      const budgetArg = budget.trim().length > 0 ? Number(budget) : null;
      await runTeam(team.id, goal.trim(), selectedModel, repoArg, budgetArg);
      pushToast({
        title: "Team run started",
        body: `“${team.name}” is planning — watch the worker cards for live progress.`,
        kind: "success",
      });
      await onStarted();
    } catch (e) {
      setError(humanizeError(e));
      setSubmitting(false);
    }
  };

  return (
    <div className="orch-modal-backdrop" onClick={onClose}>
      <div
        className="orch-modal"
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="orch-modal-head">
          <strong>Assign goal — {team.name}</strong>
          <button className="link-btn" onClick={onClose}>×</button>
        </div>
        <div className="orch-modal-body">
          <label className="orch-field">
            <span>Goal</span>
            <textarea
              className="orch-goal-input"
              autoFocus
              rows={4}
              value={goal}
              onChange={(e) => setGoal(e.target.value)}
              placeholder="e.g. Refactor the checkout flow: extract the cart adapter, add tests, review the diff."
            />
          </label>
          <label className="orch-field">
            <span>
              Repo for code tasks <span className="muted">(optional)</span>
            </span>
            <input
              className="orch-repo-input"
              type="text"
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
              placeholder="owner/repo (e.g. NetworkBound/cortex)"
              spellCheck={false}
            />
          </label>
          <label className="orch-field">
            <span>
              Soft budget (USD) <span className="muted">(optional)</span>
            </span>
            <input
              className="orch-budget-input"
              type="number"
              min="0"
              step="0.10"
              inputMode="decimal"
              value={budget}
              onChange={(e) => setBudget(e.target.value)}
              placeholder="e.g. 1.00 — flags an over-budget projection, never blocks"
              spellCheck={false}
            />
            {budgetInvalid ? (
              <span className="orch-field-warn">Enter a non-negative dollar amount.</span>
            ) : null}
          </label>
          <div className="muted orch-assign-hint">
            The manager ({team.manager_role}) breaks the goal into one task per
            worker, then all {team.workers.length} worker
            {team.workers.length === 1 ? "" : "s"} execute
            {team.workers.length === 1 ? "s" : ""} through{" "}
            {selectedModel ? <code>{selectedModel}</code> : "your default model"}.
            {repo.trim()
              ? " Code-tagged tasks edit the repo in their own gateway worktree lane — track them in the Lanes tab."
              : " Without a repo, code tasks are answered as text."}{" "}
            Each run lands as an openable transcript.
          </div>
          {error ? <div className="orch-error">{error}</div> : null}
        </div>
        <div className="orch-modal-foot">
          <button className="orch-cancel-btn" onClick={onClose} disabled={submitting}>
            Cancel
          </button>
          <button
            className="orch-submit-btn"
            disabled={!canSubmit}
            onClick={handleSubmit}
          >
            {submitting ? "Starting…" : "Start run"}
          </button>
        </div>
      </div>
    </div>
  );
}

interface CreateTeamModalProps {
  existingNames: Set<string>;
  onClose: () => void;
  onCreated: (id: string) => void | Promise<void>;
}

function CreateTeamModal({ existingNames, onClose, onCreated }: CreateTeamModalProps) {
  const [name, setName] = useState("");
  const [managerRole, setManagerRole] = useState("");
  const [workerSelection, setWorkerSelection] = useState<Set<string>>(new Set());
  const [roles, setRoles] = useState<Role[]>([]);
  const [budget, setBudget] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    listRoles()
      .then((rs) => {
        if (!active) return;
        setRoles(rs);
        // Seed sensible defaults so the picker isn't a blank slate.
        const archy =
          rs.find((r) => /architect|orchestrator|manager/i.test(r.name))?.name ??
          rs[0]?.name ?? "";
        setManagerRole(archy);
      })
      .catch((e) => {
        if (active) setError(humanizeError(e));
      });
    return () => {
      active = false;
    };
  }, []);

  // ESC closes — standard expectation for transient surfaces, matches the
  // pattern used by IDEExportModal / KeyVaultPanel.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const toggleWorker = (roleName: string) => {
    setWorkerSelection((prev) => {
      const next = new Set(prev);
      if (next.has(roleName)) next.delete(roleName);
      else next.add(roleName);
      return next;
    });
  };

  const budgetInvalid =
    budget.trim().length > 0 && !(Number(budget) >= 0 && Number.isFinite(Number(budget)));
  const canSubmit =
    !submitting &&
    name.trim().length > 0 &&
    managerRole.trim().length > 0 &&
    !existingNames.has(name.trim()) &&
    !budgetInvalid;

  const handleSubmit = async () => {
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      const team = await createTeam(
        name.trim(),
        managerRole.trim(),
        Array.from(workerSelection),
        budget.trim().length > 0 ? Number(budget) : null,
      );
      await onCreated(team.id);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="orch-modal-backdrop" onClick={onClose}>
      <div
        className="orch-modal"
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="orch-modal-head">
          <strong>New team</strong>
          <button className="link-btn" onClick={onClose}>×</button>
        </div>
        <div className="orch-modal-body">
          <label className="orch-field">
            <span>Name</span>
            <input
              type="text"
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="checkout-refactor"
            />
            {existingNames.has(name.trim()) && name.trim().length > 0 ? (
              <span className="orch-field-warn">A team with that name already exists.</span>
            ) : null}
          </label>

          <label className="orch-field">
            <span>Manager role</span>
            <select
              value={managerRole}
              onChange={(e) => setManagerRole(e.target.value)}
            >
              {roles.length === 0 ? <option value="">(no roles available)</option> : null}
              {roles.map((r) => (
                <option key={r.name} value={r.name}>{r.name}</option>
              ))}
            </select>
          </label>

          <fieldset className="orch-field">
            <legend>Worker roles</legend>
            {roles.length === 0 ? (
              <div className="muted">No roles defined yet — manage them under Roles.</div>
            ) : (
              <div className="orch-checklist">
                {roles.map((r) => (
                  <label key={r.name} className="orch-check">
                    <input
                      type="checkbox"
                      checked={workerSelection.has(r.name)}
                      onChange={() => toggleWorker(r.name)}
                    />
                    <span>{r.name}</span>
                    {r.description ? (
                      <span className="muted orch-check-desc">{r.description}</span>
                    ) : null}
                  </label>
                ))}
              </div>
            )}
          </fieldset>

          <label className="orch-field">
            <span>
              Soft budget (USD) <span className="muted">(optional)</span>
            </span>
            <input
              className="orch-budget-input"
              type="number"
              min="0"
              step="0.10"
              inputMode="decimal"
              value={budget}
              onChange={(e) => setBudget(e.target.value)}
              placeholder="e.g. 1.00 — a spend ceiling the dashboard flags, never enforced"
              spellCheck={false}
            />
            {budgetInvalid ? (
              <span className="orch-field-warn">Enter a non-negative dollar amount.</span>
            ) : null}
          </label>

          {error ? <div className="orch-error">{error}</div> : null}
        </div>
        <div className="orch-modal-foot">
          <button className="orch-cancel-btn" onClick={onClose} disabled={submitting}>
            Cancel
          </button>
          <button
            className="orch-submit-btn"
            disabled={!canSubmit}
            onClick={handleSubmit}
          >
            {submitting ? "Creating…" : "Create team"}
          </button>
        </div>
      </div>
    </div>
  );
}
