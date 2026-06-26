import { useEffect, useMemo, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { Trophy, Swords, Download, Settings } from "lucide-react";
import { useCortexStore } from "@/state/store";
import {
  arenaLeaderboard,
  arenaSend,
  arenaVote,
  formatLatency,
  formatRecord,
  type ArenaRun,
  type ModelRating,
  type ModelTurn,
} from "@/lib/model-arena";
import { listModels, type ModelEntry } from "@/lib/models";
import { MarkdownView } from "./MarkdownView";
import { pushToast } from "@/lib/toast";

const MAX_MODELS = 4;
const MIN_MODELS = 2;

/** Short, friendly label for the adapter that served an arena leg. */
function adapterLabel(adapter: string): string {
  switch (adapter) {
    case "claude-cli":
      return "claude";
    case "gateway-remote":
      return "gateway";
    case "ollama":
      return "ollama";
    default:
      return adapter;
  }
}

/** Module-scoped slot the slash command writes into before switching tabs. */
let pendingPrompt: string | null = null;

/** Called by `/arena <prompt>` to pre-fill the textarea on next mount. */
export function setArenaPreload(prompt: string): void {
  pendingPrompt = prompt;
}

export function ArenaPane() {
  const [availableModels, setAvailableModels] = useState<ModelEntry[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const [prompt, setPrompt] = useState<string>(() => {
    // Consume any pending /arena <prompt> preload exactly once.
    const p = pendingPrompt;
    pendingPrompt = null;
    return p ?? "";
  });
  const [run, setRun] = useState<ArenaRun | null>(null);
  const [sending, setSending] = useState(false);
  const [voted, setVoted] = useState(false);
  const [leaderboard, setLeaderboard] = useState<ModelRating[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [modelsLoading, setModelsLoading] = useState(true);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const setShowSettings = useCortexStore((s) => s.setShowSettings);

  // ---- Boot: fetch models + leaderboard, side-effect-free ----
  useEffect(() => {
    let mounted = true;
    (async () => {
      try {
        // Load the aggregated list (Claude CLI / Cortex Gateway / Ollama) so the arena
        // chips cover every source — arena_send executes each id through its
        // own adapter.
        const models = await listModels();
        if (mounted) setAvailableModels(models);
      } catch (e) {
        if (mounted) setError(humanizeError(e));
      } finally {
        if (mounted) setModelsLoading(false);
      }
    })();
    (async () => {
      try {
        const lb = await arenaLeaderboard();
        if (mounted) setLeaderboard(lb);
      } catch {
        /* empty leaderboard is fine */
      }
    })();
    return () => {
      mounted = false;
    };
  }, []);

  const allFinished = useMemo(() => {
    if (!run) return false;
    return run.models.every((m) => !!m.response || !!m.error);
  }, [run]);

  function toggleModel(id: string) {
    setSelected((prev) => {
      if (prev.includes(id)) return prev.filter((m) => m !== id);
      if (prev.length >= MAX_MODELS) {
        pushToast({
          title: "Arena cap",
          body: `Max ${MAX_MODELS} models per duel.`,
          kind: "warning",
        });
        return prev;
      }
      return [...prev, id];
    });
  }

  async function handleSend() {
    if (selected.length < MIN_MODELS) {
      pushToast({
        title: "Pick more models",
        body: `Select at least ${MIN_MODELS} models to compare.`,
        kind: "warning",
      });
      return;
    }
    const text = prompt.trim();
    if (!text) {
      pushToast({ title: "Empty prompt", body: "Type something to send.", kind: "warning" });
      return;
    }
    setSending(true);
    setError(null);
    setVoted(false);
    // Seed the run with empty turns so the columns render immediately.
    setRun({
      run_id: "pending",
      models: selected.map((m) => ({
        model: m,
        adapter: "",
        response: "",
        tokens: 0,
        latency_ms: 0,
        error: null,
      })),
    });
    try {
      const result = await arenaSend(text, selected);
      setRun(result);
    } catch (e) {
      setError(humanizeError(e));
      setRun(null);
    } finally {
      setSending(false);
    }
  }

  async function handleVote(winner: string) {
    if (!run) return;
    const losers = run.models.map((m) => m.model).filter((m) => m !== winner);
    try {
      const update = await arenaVote(run.run_id, winner, losers);
      setLeaderboard(update.ratings);
      setVoted(true);
      pushToast({ title: "Vote recorded", body: `${winner} wins this round.`, kind: "success" });
    } catch (e) {
      pushToast({ title: "Vote failed", body: humanizeError(e), kind: "error" });
    }
  }

  function handleSkipVote() {
    setVoted(true);
    pushToast({ title: "Vote skipped", body: "No ELO update applied.", kind: "info" });
  }

  return (
    <div className="arena-pane">
      <div className="arena-layout">
        <div className="arena-main">
        {error && <div className="arena-error">{error}</div>}
        {!modelsLoading && availableModels.length === 0 && !error ? (
          <ArenaNoModels
            singleModel={false}
            onCookbook={() => setActivityTab("cookbook")}
            onSettings={() => setShowSettings(true)}
          />
        ) : !modelsLoading && availableModels.length === 1 ? (
          <ArenaNoModels
            singleModel
            onCookbook={() => setActivityTab("cookbook")}
            onSettings={() => setShowSettings(true)}
          />
        ) : null}
        <ModelPicker
          available={availableModels}
          selected={selected}
          onToggle={toggleModel}
          loading={modelsLoading}
        />
        <PromptBar
          prompt={prompt}
          onChange={setPrompt}
          onSend={handleSend}
          sending={sending}
          canSend={selected.length >= MIN_MODELS}
        />
        {run && (
          <ArenaGrid
            run={run}
            sending={sending}
            allFinished={allFinished}
            voted={voted}
            onVote={handleVote}
            onSkip={handleSkipVote}
          />
        )}
        {!run && !sending && !error && availableModels.length > 0 && (
          <div className="arena-empty muted">
            Pick {MIN_MODELS}-{MAX_MODELS} models, type a prompt, and hit Compare to start a duel.
          </div>
        )}
        </div>
        <Leaderboard ratings={leaderboard} />
      </div>
    </div>
  );
}

function ArenaNoModels({
  singleModel,
  onCookbook,
  onSettings,
}: {
  singleModel: boolean;
  onCookbook: () => void;
  onSettings: () => void;
}) {
  return (
    <div className="arena-gateway-notice" role="status">
      <Swords size={16} strokeWidth={1.9} aria-hidden="true" />
      <div className="arena-gateway-copy">
        <strong>
          {singleModel
            ? "One more model to start a duel"
            : "The arena needs models to compare"}
        </strong>
        <span>
          {singleModel
            ? `Arena compares ${MIN_MODELS}–${MAX_MODELS} models head-to-head. Pull a local model or add a provider key to line up a second contender.`
            : `Arena runs the same prompt across ${MIN_MODELS}–${MAX_MODELS} models side-by-side. Pull a local model in the Cookbook or add a provider key in Settings, then come back to compare.`}
        </span>
      </div>
      <div className="arena-gateway-actions">
        <button type="button" className="arena-gateway-btn" onClick={onCookbook}>
          <Download size={13} strokeWidth={1.9} aria-hidden="true" />
          Cookbook
        </button>
        <button type="button" className="arena-gateway-btn" onClick={onSettings}>
          <Settings size={13} strokeWidth={1.9} aria-hidden="true" />
          Settings
        </button>
      </div>
    </div>
  );
}

function ModelPicker({
  available,
  selected,
  onToggle,
  loading,
}: {
  available: ModelEntry[];
  selected: string[];
  onToggle: (id: string) => void;
  loading: boolean;
}) {
  if (available.length === 0) {
    // Only advertise "loading" while the fetch is genuinely in flight — once it
    // settles (success-but-empty or a surfaced load error) drop the spinner copy
    // so it never stacks under the error box.
    if (loading) return <div className="arena-picker muted">Loading models…</div>;
    return null;
  }
  return (
    <div className="arena-picker">
      <div className="arena-picker-label">
        Models{" "}
        <span className="muted">
          ({selected.length}/{MAX_MODELS})
        </span>
      </div>
      <div className="arena-chips">
        {available.map((m) => {
          const isOn = selected.includes(m.id);
          return (
            <button
              key={m.id}
              type="button"
              className={`arena-chip${isOn ? " active" : ""}`}
              onClick={() => onToggle(m.id)}
              title={`${m.label} · ${m.source}`}
            >
              {m.label}
            </button>
          );
        })}
      </div>
    </div>
  );
}

function PromptBar({
  prompt,
  onChange,
  onSend,
  sending,
  canSend,
}: {
  prompt: string;
  onChange: (v: string) => void;
  onSend: () => void;
  sending: boolean;
  canSend: boolean;
}) {
  return (
    <div className="arena-prompt">
      <textarea
        value={prompt}
        onChange={(e) => onChange(e.target.value)}
        placeholder="Ask both models the same question…"
        rows={3}
        disabled={sending}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            onSend();
          }
        }}
      />
      <button
        type="button"
        className="arena-send-btn"
        onClick={onSend}
        disabled={sending || !canSend}
      >
        {sending ? "Comparing…" : "Compare"}
      </button>
    </div>
  );
}

function ArenaGrid({
  run,
  sending,
  allFinished,
  voted,
  onVote,
  onSkip,
}: {
  run: ArenaRun;
  sending: boolean;
  allFinished: boolean;
  voted: boolean;
  onVote: (winner: string) => void;
  onSkip: () => void;
}) {
  const n = run.models.length;
  // Bound the column count to 4 so a 4-way duel still reads on a 14" screen.
  const cols = Math.min(n, 4);
  return (
    <div className="arena-grid-wrap">
      <div className="arena-grid" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}>
        {run.models.map((turn) => (
          <ArenaColumn
            key={turn.model}
            turn={turn}
            sending={sending}
            canVote={allFinished && !voted && !turn.error}
            onVote={() => onVote(turn.model)}
          />
        ))}
      </div>
      {allFinished && !voted && (
        <div className="arena-vote-bar">
          <span className="muted">Pick a winner to update ELO ratings:</span>
          <button type="button" className="arena-skip-btn" onClick={onSkip}>
            Skip vote
          </button>
        </div>
      )}
    </div>
  );
}

function ArenaColumn({
  turn,
  sending,
  canVote,
  onVote,
}: {
  turn: ModelTurn;
  sending: boolean;
  canVote: boolean;
  onVote: () => void;
}) {
  const pending = sending && !turn.response && !turn.error;
  return (
    <div className={`arena-col${turn.error ? " errored" : ""}`}>
      <div className="arena-col-head">
        <strong className="arena-col-model">{turn.model}</strong>
        {turn.adapter && (
          <span className="arena-col-adapter" title={`served by ${turn.adapter}`}>
            {adapterLabel(turn.adapter)}
          </span>
        )}
        {turn.latency_ms > 0 && (
          <span className="muted arena-col-stats">
            {formatLatency(turn.latency_ms)} · {turn.tokens} tok
          </span>
        )}
      </div>
      <div className="arena-col-body">
        {pending && <div className="muted">streaming…</div>}
        {turn.error && <div className="arena-error">{turn.error}</div>}
        {!pending && turn.response && (
          // Markdown, not raw text — fences/headers/lists render properly so
          // side-by-side quality judging actually works (same renderer as the
          // chat's inline compare mode).
          <MarkdownView source={turn.response} />
        )}
      </div>
      {canVote && (
        <button type="button" className="arena-winner-btn" onClick={onVote}>
          <Trophy size={14} strokeWidth={1.75} aria-hidden="true" /> Winner
        </button>
      )}
    </div>
  );
}

function Leaderboard({ ratings }: { ratings: ModelRating[] }) {
  return (
    <aside className="arena-leaderboard">
      <div className="arena-leaderboard-head">
        <Trophy size={13} strokeWidth={1.75} aria-hidden="true" /> Leaderboard
      </div>
      {ratings.length === 0 ? (
        <div className="muted" style={{ padding: 8 }}>No votes yet.</div>
      ) : (
        <table className="arena-leaderboard-table">
          <thead>
            <tr>
              <th>#</th>
              <th>Model</th>
              <th>ELO</th>
              <th>W-L</th>
            </tr>
          </thead>
          <tbody>
            {ratings.map((r, i) => (
              <tr key={r.model}>
                <td className="muted">{i + 1}</td>
                <td className="arena-lb-model" title={r.model}>{r.model}</td>
                <td>{Math.round(r.rating)}</td>
                <td className="muted">{formatRecord(r)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </aside>
  );
}
