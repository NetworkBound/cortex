import { useEffect, useState } from "react";
import { getSessions } from "../lib/api";
import { useStore } from "../lib/store";
import type { SessionSummary } from "../lib/types";

/** Compact relative time, e.g. "just now", "5m", "3h", "2d", "Apr 4". */
function relativeTime(ms: number): string {
  const diff = Date.now() - ms;
  if (!Number.isFinite(diff) || diff < 0) return "";
  const s = Math.floor(diff / 1000);
  if (s < 45) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d`;
  return new Date(ms).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

/** Recent chat sessions — tap one to load its history into the Chat view and
 *  resume that conversation. Handed off to the Chat view via the store's
 *  `openSession`; the parent (`App`) also switches to the Chat tab. */
export default function RecentView({
  onOpen,
  onImport,
  refreshKey,
}: {
  onOpen?: () => void;
  /** Open the Import sub-screen. */
  onImport?: () => void;
  /** Bump to force a reload (e.g. after an import completes). */
  refreshKey?: number;
}) {
  const { setOpenSession, wsStatus } = useStore();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = () => {
    setLoading(true);
    getSessions()
      .then((s) => {
        setSessions(Array.isArray(s) ? s : []);
        setError(null);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  };

  useEffect(refresh, []);
  // Refresh when the WS (re)connects so a freshly-finished chat shows up.
  useEffect(() => {
    if (wsStatus === "open") refresh();
  }, [wsStatus]);
  // Refresh when the parent bumps `refreshKey` (e.g. after an import).
  useEffect(() => {
    if (refreshKey) refresh();
  }, [refreshKey]);

  const open = (id: string) => {
    setOpenSession(id);
    onOpen?.();
  };

  return (
    <div className="scroll">
      <div className="pad recent-top">
        <button className="btn import-cta" style={{ width: "100%" }} onClick={onImport}>
          ＋ Import chat history
        </button>
      </div>

      {error && <div className="banner err">{error}</div>}
      {loading && sessions.length === 0 && (
        <div className="empty">Loading chats…</div>
      )}
      {!loading && sessions.length === 0 && !error && (
        <div className="empty">No chats yet. Start one in the Chat tab.</div>
      )}

      <div className="list">
        {sessions.map((s) => (
          <button
            key={s.id}
            className="row-item"
            onClick={() => open(s.id)}
          >
            <div className="meta">
              <div className="name">{s.title || "New chat"}</div>
              {s.preview && <div className="sub">{s.preview}</div>}
            </div>
            <div className="session-aside">
              <div className="session-time">{relativeTime(s.last_ts)}</div>
              <div className="session-count">{s.message_count} msg</div>
            </div>
          </button>
        ))}
      </div>

      {sessions.length > 0 && (
        <div className="pad">
          <button className="btn" style={{ width: "100%" }} onClick={refresh}>
            Refresh
          </button>
        </div>
      )}
    </div>
  );
}
