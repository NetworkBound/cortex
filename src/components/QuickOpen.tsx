import { useEffect, useMemo, useRef, useState } from "react";
import { brainSnapshot, type RecentSession } from "@/lib/brain";
import { searchMemory, type MemorySearchHit } from "@/lib/memory";
import { timeAgo } from "@/lib/time";
import { projectFiles, type FileTreeEntry } from "@/lib/projects";
import { loadSessionMessages } from "@/lib/sessions";
import { useCortexStore, type Message } from "@/state/store";

/**
 * Ctrl+P quick-open palette. Mirrors VS Code: a single fuzzy search across
 * project files, memory entries (when query is non-empty), and recent
 * sessions. Distinct from CommandPalette (Ctrl+K) which lists actions.
 */

type Row =
  | { kind: "file"; key: string; title: string; secondary: string; path: string }
  | { kind: "memory"; key: string; title: string; secondary: string; hit: MemorySearchHit }
  | { kind: "session"; key: string; title: string; secondary: string; session: RecentSession };

const MAX_RESULTS = 30;
const FILE_LIMIT = 200;
const SESSION_LIMIT = 10;
const MEMORY_LIMIT = 10;

export function QuickOpen() {
  const open = useCortexStore((s) => s.showQuickOpen);
  const setOpen = useCortexStore((s) => s.setShowQuickOpen);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const resume = useCortexStore((s) => s.resumeSession);
  const activeProject = useCortexStore((s) => s.activeProject);

  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);
  const [files, setFiles] = useState<FileTreeEntry[]>([]);
  const [sessions, setSessions] = useState<RecentSession[]>([]);
  const [memHits, setMemHits] = useState<MemorySearchHit[]>([]);
  const [toast, setToast] = useState<string | null>(null);
  const memToken = useRef(0);

  useEffect(() => {
    if (!open) return;
    setQ("");
    setIdx(0);
    setMemHits([]);
    setToast(null);

    if (activeProject?.root) {
      projectFiles(activeProject.root, FILE_LIMIT)
        .then((entries) => setFiles(entries.filter((e) => !e.is_dir)))
        .catch(() => setFiles([]));
    } else {
      setFiles([]);
    }

    brainSnapshot()
      .then((s) => setSessions((s.recent_sessions ?? []).slice(0, SESSION_LIMIT)))
      .catch(() => setSessions([]));
  }, [open, activeProject?.root]);

  // Debounced memory search while typing.
  useEffect(() => {
    if (!open) return;
    const query = q.trim();
    if (!query) { setMemHits([]); return; }
    const tok = ++memToken.current;
    const t = window.setTimeout(() => {
      searchMemory(query, {
        activeProject: activeProject?.root,
      })
        .then((hits) => {
          if (tok === memToken.current) setMemHits(hits.slice(0, MEMORY_LIMIT));
        })
        .catch(() => { if (tok === memToken.current) setMemHits([]); });
    }, 140);
    return () => window.clearTimeout(t);
  }, [q, open, activeProject?.root]);

  const allRows: Row[] = useMemo(() => {
    const out: Row[] = [];
    for (const f of files) {
      const sep = f.path.includes("\\") && !f.path.includes("/") ? "\\" : "/";
      const i = f.path.lastIndexOf(sep);
      const base = i >= 0 ? f.path.slice(i + 1) : f.path;
      const dir = i >= 0 ? f.path.slice(0, i) : "";
      out.push({ kind: "file", key: `f:${f.path}`, title: base, secondary: dir, path: f.path });
    }
    for (const h of memHits) {
      const sep = h.path.includes("\\") && !h.path.includes("/") ? "\\" : "/";
      const i = h.path.lastIndexOf(sep);
      const base = i >= 0 ? h.path.slice(i + 1) : h.path;
      out.push({
        kind: "memory",
        key: `m:${h.source}:${h.path}`,
        title: base || h.source,
        secondary: h.source,
        hit: h,
      });
    }
    for (const s of sessions) {
      out.push({
        kind: "session",
        key: `s:${s.session_id}`,
        title: s.first_message?.trim() || `session ${s.session_id.slice(-8)}`,
        secondary: `${timeAgo(s.last_active_ms)} · ${s.message_count} msgs`,
        session: s,
      });
    }
    return out;
  }, [files, memHits, sessions]);

  const filtered = useMemo(() => {
    const query = q.trim();
    if (!query) return allRows.slice(0, MAX_RESULTS);
    const scored = allRows
      .map((r) => ({ r, score: scoreRow(r, query) }))
      .filter((s) => s.score > 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, MAX_RESULTS)
      .map((s) => s.r);
    return scored;
  }, [allRows, q]);

  useEffect(() => { setIdx(0); }, [q]);

  async function activate(row: Row) {
    if (row.kind === "file") {
      setOpen(false);
      try {
        await navigator.clipboard.writeText(row.path);
        setToast("path copied");
      } catch {
        setToast("copy failed");
      }
    } else if (row.kind === "memory") {
      setOpen(false);
      setActivityTab("memory");
    } else {
      try {
        const stored = await loadSessionMessages(row.session.session_id);
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
        resume(row.session.session_id, msgs);
      } catch { /* swallow */ }
      setOpen(false);
    }
  }

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); setOpen(false); }
      else if (e.key === "ArrowDown") { e.preventDefault(); setIdx((i) => Math.min(i + 1, filtered.length - 1)); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setIdx((i) => Math.max(i - 1, 0)); }
      else if (e.key === "Enter") {
        e.preventDefault();
        const row = filtered[idx];
        if (row) void activate(row);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, filtered, idx, setOpen]);

  if (!open) return null;

  return (
    <div className="palette-backdrop" onClick={() => setOpen(false)}>
      <div className="palette quick-open" onClick={(e) => e.stopPropagation()}>
        <input
          autoFocus
          className="quick-open-input"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Quick open file, memory, or session…"
        />
        <ul className="quick-open-list">
          {filtered.length === 0 && <li className="muted">no matches</li>}
          {filtered.map((r, i) => (
            <li
              key={r.key}
              className={`quick-open-row ${i === idx ? "active" : ""}`}
              onMouseEnter={() => setIdx(i)}
              onClick={() => void activate(r)}
            >
              <span className={`quick-open-badge ${r.kind}`}>{r.kind.toUpperCase()}</span>
              <div className="quick-open-text">
                <div className="quick-open-title">{r.title}</div>
                <div className="quick-open-secondary muted">{r.secondary}</div>
              </div>
            </li>
          ))}
        </ul>
        {toast && <div className="muted" style={{ padding: 6 }}>{toast}</div>}
      </div>
    </div>
  );
}

// ----- scoring ---------------------------------------------------------------

function scoreRow(row: Row, query: string): number {
  const q = query.toLowerCase();
  const title = row.title.toLowerCase();
  const secondary = row.secondary.toLowerCase();

  let best = 0;
  best = Math.max(best, scoreText(title, q) * 2);
  best = Math.max(best, scoreText(secondary, q));
  return best;
}

function scoreText(text: string, q: string): number {
  if (!text) return 0;
  const i = text.indexOf(q);
  if (i < 0) return 0;
  let score = 10;
  // Prefix match is strongest.
  if (i === 0) score += 20;
  // Word-boundary bonus.
  else if (i > 0 && /[\s/\\._-]/.test(text[i - 1] ?? "")) score += 8;
  // Shorter haystack ranks higher (more specific match).
  score += Math.max(0, 12 - Math.floor(text.length / 8));
  // Earlier matches rank higher.
  score -= Math.min(8, i);
  return score;
}

