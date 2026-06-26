/**
 * Focus chain panel — the live to-do list for the current chat session.
 *
 * Two writers: the AGENT (its streamed ```focus-chain checklist is re-emitted
 * by the backend as an `update_focus_chain` tool call; ChatPane routes that
 * into `replaceChain`) and the USER (add a step below, tick/untick any item).
 * Both go through `lib/focus-chain.ts` mutators, which persist per session.
 *
 * On mount (and on session change) we rehydrate from disk so resuming a chat
 * brings back the previous chain.
 */

import { useEffect, useState } from "react";
import { useCortexStore } from "@/state/store";
import { addTask, clearChain, loadFocusChain, tickTask } from "@/lib/focus-chain";

export function FocusChain() {
  const sessionId = useCortexStore((s) => s.sessionId);
  const chain = useCortexStore((s) => s.focusChain);
  const setFocusChain = useCortexStore((s) => s.setFocusChain);
  const [draft, setDraft] = useState("");

  const submitDraft = () => {
    const title = draft.trim();
    if (!title) return;
    addTask(title);
    setDraft("");
  };

  // Rehydrate from disk whenever the active session changes (new thread,
  // resumeSession, etc). Cancellation guard prevents a stale load from
  // overwriting a freshly-emitted chain if the session flips mid-flight.
  useEffect(() => {
    let cancelled = false;
    void loadFocusChain(sessionId).then((items) => {
      if (cancelled) return;
      // Only overwrite if the store is still empty — otherwise the agent has
      // already updated the chain since we kicked off the load and we'd be
      // clobbering fresh data with a stale snapshot.
      const current = useCortexStore.getState().focusChain;
      if (current.length === 0 && items.length > 0) {
        setFocusChain(items);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [sessionId, setFocusChain]);

  const total = chain.length;
  const done = chain.filter((t) => t.done).length;
  const remaining = total - done;

  return (
    <div className="focus-chain">
      <div className="focus-chain-head">
        <span className="focus-chain-count">
          {total === 0 ? "no tasks" : `${done}/${total} done`}
        </span>
        {remaining > 0 && (
          <span className="focus-chain-chip">{remaining} left</span>
        )}
        {total > 0 && (
          <button
            type="button"
            className="link-btn focus-chain-clear"
            onClick={() => clearChain()}
            title="Clear the focus chain"
          >
            Clear
          </button>
        )}
      </div>
      <div className="focus-chain-body">
        {total === 0 ? (
          <div className="muted focus-chain-empty">
            No steps yet. The agent keeps this list while it works through
            multi-step tasks — or add your own steps below.
          </div>
        ) : (
          <ul className="focus-chain-list">
            {chain.map((t) => (
              <li
                key={t.id}
                className={`focus-chain-item ${t.done ? "done" : ""}`}
              >
                <input
                  type="checkbox"
                  checked={t.done}
                  onChange={(e) => tickTask(t.id, e.target.checked)}
                  aria-label={t.done ? "completed" : "pending"}
                />
                <span className="focus-chain-title">{t.title}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <form
        className="focus-chain-add"
        onSubmit={(e) => {
          e.preventDefault();
          submitDraft();
        }}
      >
        <input
          type="text"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="Add a step…"
          aria-label="Add a step to the focus chain"
        />
        <button type="submit" disabled={!draft.trim()}>
          Add
        </button>
      </form>
    </div>
  );
}
