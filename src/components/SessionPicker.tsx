import { useEffect, useMemo, useState } from "react";
import { brainSnapshot, type RecentSession } from "@/lib/brain";
import { humanizeError } from "@/lib/errors";
import { timeAgo } from "@/lib/time";
import { loadSessionMessages } from "@/lib/sessions";
import { pushToast } from "@/lib/toast";
import { useCortexStore, type Message } from "@/state/store";
import { Skeleton } from "@/components/Skeleton";

/**
 * Toad's Ctrl+R session resume picker. Lists all sessions with previews,
 * arrow keys to navigate, Enter to resume (loads chat history into the
 * current session).
 *
 * Failures are surfaced, not swallowed: a snapshot-load error renders an
 * inline error row (distinct from the genuine "no sessions yet" empty state)
 * and a failed resume keeps the picker open with an inline message + toast,
 * so a broken backend never masquerades as a no-op.
 */
export function SessionPicker() {
  const open = useCortexStore((s) => s.showSessionPicker);
  const setOpen = useCortexStore((s) => s.setShowSessionPicker);
  const resume = useCortexStore((s) => s.resumeSession);
  const [sessions, setSessions] = useState<RecentSession[]>([]);
  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);
  const [loading, setLoading] = useState(false);
  /** brainSnapshot failed — the list is unknown, not empty. */
  const [listError, setListError] = useState<string | null>(null);
  /** The last clicked session failed to load. */
  const [pickError, setPickError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setQ("");
    setIdx(0);
    setListError(null);
    setPickError(null);
    brainSnapshot()
      .then((s) => {
        setSessions(s.recent_sessions);
        setListError(null);
      })
      .catch((err) => {
        console.warn("session picker: snapshot load failed", err);
        setSessions([]);
        setListError(humanizeError(err));
      });
  }, [open]);

  const filtered = useMemo(() => {
    if (!q.trim()) return sessions;
    const lc = q.toLowerCase();
    return sessions.filter(
      (s) =>
        (s.first_message ?? "").toLowerCase().includes(lc) ||
        s.session_id.toLowerCase().includes(lc) ||
        s.agents.some((a) => a.toLowerCase().includes(lc)),
    );
  }, [sessions, q]);

  async function pick(s: RecentSession) {
    setLoading(true);
    setPickError(null);
    try {
      const stored = await loadSessionMessages(s.session_id);
      const msgs: Message[] = stored.map((m) => ({
        id: m.id,
        role: (m.role as Message["role"]) || "assistant",
        agent: m.agent_id ?? undefined,
        content: m.content,
        reasoning: m.reasoning ?? undefined,
        pending: false,
        tools: [],
        runId: m.run_id,
      }));
      resume(s.session_id, msgs);
      setOpen(false);
    } catch (err) {
      // Keep the picker open so the user can retry or pick another session;
      // a silent close here would make a failed resume look like a no-op.
      console.warn("session picker: resume failed", err);
      setPickError(humanizeError(err));
      pushToast({
        title: "Couldn't open that session",
        body: humanizeError(err),
        kind: "error",
      });
    }
    finally { setLoading(false); }
  }

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); setOpen(false); }
      else if (e.key === "ArrowDown") { e.preventDefault(); setIdx((i) => Math.min(i + 1, filtered.length - 1)); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setIdx((i) => Math.max(i - 1, 0)); }
      else if (e.key === "Enter") {
        e.preventDefault();
        if (filtered[idx]) void pick(filtered[idx]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, filtered, idx, setOpen]);

  if (!open) return null;
  return (
    <div className="palette-backdrop" onClick={() => setOpen(false)}>
      <div className="palette session-picker" onClick={(e) => e.stopPropagation()}>
        <input
          autoFocus
          value={q}
          onChange={(e) => { setQ(e.target.value); setIdx(0); }}
          placeholder="Resume a chat session…"
        />
        {pickError && (
          <div className="session-picker-error" role="alert">
            <strong>Couldn't open that session.</strong> {pickError} Pick
            another session, or try again.
          </div>
        )}
        <ul>
          {listError && filtered.length === 0 && !loading && (
            <li className="session-picker-error" role="alert">
              Couldn't load your sessions — {listError}
            </li>
          )}
          {!listError && filtered.length === 0 && !loading && (
            <li className="muted">no sessions yet</li>
          )}
          {loading &&
            filtered.length === 0 &&
            Array.from({ length: 5 }).map((_, i) => (
              <li key={`sk-${i}`} className="sp-skeleton" aria-hidden="true">
                <div className="sp-row">
                  <Skeleton variant="text" width="60%" />
                </div>
                <Skeleton
                  variant="text"
                  width="35%"
                  style={{ marginTop: 6, height: "0.6em" }}
                />
              </li>
            ))}
          {filtered.map((s, i) => (
            <li
              key={s.session_id}
              className={i === idx ? "active" : ""}
              onMouseEnter={() => setIdx(i)}
              onClick={() => void pick(s)}
            >
              <div className="sp-row">
                <strong className="sp-title">
                  {s.first_message ?? `session ${s.session_id.slice(-8)}`}
                </strong>
                <span className="muted">{timeAgo(s.last_active_ms)}</span>
              </div>
              <div className="sp-meta muted">
                {s.message_count} msgs
                {s.agents.length > 0 && ` · ${s.agents.filter(Boolean).join(", ")}`}
              </div>
            </li>
          ))}
        </ul>
        {loading && filtered.length > 0 && (
          <div className="muted" style={{ padding: 6 }}>loading…</div>
        )}
      </div>
    </div>
  );
}

