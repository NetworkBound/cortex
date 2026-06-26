import { useCallback, useEffect, useState } from "react";
import { getApprovals, resolveApproval } from "../lib/api";
import { useWs } from "../lib/useWs";
import type { Approval } from "../lib/types";

export default function InboxView() {
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [reasons, setReasons] = useState<Record<string, string>>({});

  const refresh = useCallback(() => {
    getApprovals()
      .then((a) => {
        setApprovals(Array.isArray(a) ? a : []);
        setError(null);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
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

  return (
    <div className="scroll">
      {error && <div className="banner err">{error}</div>}
      {approvals.length === 0 ? (
        <div className="empty">No pending approvals.</div>
      ) : (
        approvals.map((a) => (
          <div className="approval" key={a.id}>
            <div className="card-title" style={{ marginBottom: 4 }}>
              🔒 {a.tool || "approval"}
            </div>
            {a.preview && <div className="preview">{a.preview}</div>}
            <div className="faint" style={{ fontSize: 11 }}>
              run {a.run_id}
            </div>
            <input
              className="reason"
              placeholder="Reason (optional)"
              value={reasons[a.id] ?? ""}
              onChange={(e) =>
                setReasons((r) => ({ ...r, [a.id]: e.target.value }))
              }
            />
            <div className="actions">
              <button
                className="btn approve"
                disabled={busy.has(a.id)}
                onClick={() => decide(a, true)}
              >
                {busy.has(a.id) ? "…" : "Approve"}
              </button>
              <button
                className="btn reject"
                disabled={busy.has(a.id)}
                onClick={() => decide(a, false)}
              >
                {busy.has(a.id) ? "…" : "Reject"}
              </button>
            </div>
          </div>
        ))
      )}
    </div>
  );
}
