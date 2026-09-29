import { useCallback, useEffect, useState } from "react";
import { getApprovals, resolveApproval } from "../lib/api";
import { useWs } from "../lib/useWs";
import type { Approval } from "../lib/types";

/** Shorten a run id for the meta line (`run 3f9a…c21e`). */
function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 4)}…${id.slice(-4)}` : id;
}

/**
 * Approval inbox: every pending tool approval from the desktop's runs, each
 * with the exact command/preview, an optional reason and Approve / Reject.
 * Rows only leave the list once the server confirms the decision.
 */
export default function InboxView() {
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [reasons, setReasons] = useState<Record<string, string>>({});

  const refresh = useCallback(() => {
    getApprovals()
      .then((a) => {
        setApprovals(Array.isArray(a) ? a : []);
        setError(null);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoaded(true));
  }, []);

  // Poll every few seconds.
  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 4000);
    return () => clearInterval(t);
  }, [refresh]);

  // React to approval-related WS frames.
  useWs((f) => {
    if (f.type === "chat_approval" || f.type === "chat_approval_resolved") {
      refresh();
    }
  });

  const decide = async (a: Approval, approve: boolean) => {
    if (busy.has(a.id)) return;
    setBusy((b) => new Set(b).add(a.id));
    try {
      await resolveApproval(a.id, approve, reasons[a.id]?.trim() || undefined);
      // NO optimistic UI: only drop the row after the request succeeds.
      setApprovals((list) => list.filter((x) => x.id !== a.id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy((b) => {
        const n = new Set(b);
        n.delete(a.id);
        return n;
      });
    }
  };

  const count = approvals.length;

  return (
    <div className="scroll">
      <div className="inbox-head">
        <h2>
          {count === 0
            ? "Approvals"
            : `${count} pending ${count === 1 ? "approval" : "approvals"}`}
        </h2>
        <button className="btn small" onClick={refresh} aria-label="Refresh">
          Refresh
        </button>
      </div>
      {error && (
        <div className="banner err" role="alert">
          {error}
        </div>
      )}
      {loaded && count === 0 && !error ? (
        <div className="empty">
          No pending approvals.
          <div className="empty-hint">
            When a desktop run needs your say-so on a command or file write, it
            shows up here — approve or reject from your phone.
          </div>
        </div>
      ) : (
        approvals.map((a) => {
          const inFlight = busy.has(a.id);
          return (
            <section className="approval" key={a.id} aria-busy={inFlight}>
              <div className="card-title">
                <span className="tool">🔒 {a.tool || "approval"}</span>
                <span className="meta-line">run {shortId(a.run_id)}</span>
              </div>
              {a.preview && <div className="preview">{a.preview}</div>}
              <input
                className="reason"
                placeholder="Reason (optional)"
                aria-label="Reason for your decision (optional)"
                value={reasons[a.id] ?? ""}
                disabled={inFlight}
                onChange={(e) =>
                  setReasons((r) => ({ ...r, [a.id]: e.target.value }))
                }
              />
              <div className="actions">
                <button
                  className="btn approve"
                  disabled={inFlight}
                  onClick={() => decide(a, true)}
                >
                  {inFlight ? "…" : "Approve"}
                </button>
                <button
                  className="btn reject"
                  disabled={inFlight}
                  onClick={() => decide(a, false)}
                >
                  {inFlight ? "…" : "Reject"}
                </button>
              </div>
            </section>
          );
        })
      )}
    </div>
  );
}
