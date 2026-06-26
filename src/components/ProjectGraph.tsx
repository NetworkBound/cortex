// ProjectGraph — force-directed graph of projects + sessions + memory.
//
// WIRING (do this in src/components/BrainPanel.tsx after this file lands):
//   1. Add "graph" to the Tab union:
//        type Tab = "everything" | "sessions" | "projects" | "memory" | "graph" | "usage";
//   2. Import: `import { ProjectGraph } from "./ProjectGraph";`
//   3. Add a tab button between the "memory" and "usage" buttons:
//        <button className={tab === "graph" ? "active" : ""} onClick={() => setTab("graph")}>
//          graph
//        </button>
//   4. Render the panel:
//        {tab === "graph" && <ProjectGraph />}
//   No changes needed in App.tsx.

import { useEffect, useMemo, useRef, useState } from "react";
import {
  brainSnapshot,
  type BrainSnapshot,
  type RecentMemory,
  type RecentProject,
  type RecentSession,
} from "@/lib/brain";
import { bootstrapProjectSession, loadSessionMessages } from "@/lib/sessions";
import { PanelLoading } from "./Skeleton";
import { setActiveProject } from "@/lib/projects";
import { openInEditor } from "@/lib/editor";
import { useCortexStore, type Message } from "@/state/store";

type NodeKind = "project" | "session" | "memory";

interface GNode {
  id: string;
  kind: NodeKind;
  label: string;
  meta: string;
  size: number;
  x: number;
  y: number;
  vx: number;
  vy: number;
  ref:
    | { kind: "project"; data: RecentProject }
    | { kind: "session"; data: RecentSession }
    | { kind: "memory"; data: RecentMemory };
}

interface GEdge {
  a: string;
  b: string;
}

const VIEW_W = 1000;
const VIEW_H = 700;
const CX = VIEW_W / 2;
const CY = VIEW_H / 2;

// Physics tuning (kept gentle; this is approximate, not exact).
const REPULSION = 1800;
const SPRING_K = 0.012;
const SPRING_LEN = 90;
const GRAVITY = 0.012;
const DAMPING = 0.92;

function basename(p: string): string {
  const m = p.match(/([^/\\]+)$/);
  return m ? m[1] : p;
}

function truncate(s: string, n: number): string {
  if (s.length <= n) return s;
  return s.slice(0, Math.max(0, n - 1)) + "…";
}

function buildGraph(snap: BrainSnapshot): { nodes: GNode[]; edges: GEdge[] } {
  const nodes: GNode[] = [];
  const edges: GEdge[] = [];
  const projects = snap.recent_projects;
  const sessions = snap.recent_sessions;
  const memory = snap.recent_memory;

  // Seed positions in a loose circle so the sim has somewhere to start.
  const total = projects.length + sessions.length + memory.length || 1;
  let i = 0;
  const seed = (): { x: number; y: number } => {
    const t = (i / total) * Math.PI * 2;
    i += 1;
    const r = 200 + ((i * 37) % 120);
    return { x: CX + Math.cos(t) * r, y: CY + Math.sin(t) * r };
  };

  for (const p of projects) {
    const s = seed();
    nodes.push({
      id: `p:${p.root}`,
      kind: "project",
      label: p.name,
      meta: `${p.root}`,
      size: 14,
      x: s.x,
      y: s.y,
      vx: 0,
      vy: 0,
      ref: { kind: "project", data: p },
    });
  }

  for (const s of sessions) {
    const pos = seed();
    nodes.push({
      id: `s:${s.session_id}`,
      kind: "session",
      label: s.first_message ?? `session ${s.session_id.slice(-8)}`,
      meta: `${s.message_count} msgs · ${s.agents.filter(Boolean).join(", ") || "—"}`,
      size: 8,
      x: pos.x,
      y: pos.y,
      vx: 0,
      vy: 0,
      ref: { kind: "session", data: s },
    });
  }

  for (const m of memory) {
    const pos = seed();
    nodes.push({
      id: `m:${m.path}`,
      kind: "memory",
      label: m.title ?? basename(m.path),
      meta: `${m.source}`,
      size: 5,
      x: pos.x,
      y: pos.y,
      vx: 0,
      vy: 0,
      ref: { kind: "memory", data: m },
    });
  }

  // Edges: session→project when memory.path mentions project.name (best-effort
  // proxy — we don't have direct session↔project links, so we use the same
  // heuristic as memory: a session that ran inside a project tends to show up
  // alongside memory under that project's name).
  for (const p of projects) {
    const projNameLower = p.name.toLowerCase();
    const projRootLower = p.root.toLowerCase();
    for (const s of sessions) {
      const blob = `${s.first_message ?? ""} ${s.agents.join(" ")}`.toLowerCase();
      if (blob.includes(projNameLower)) {
        edges.push({ a: `s:${s.session_id}`, b: `p:${p.root}` });
      }
    }
    for (const m of memory) {
      const pathLower = m.path.toLowerCase();
      const sourceLower = m.source.toLowerCase();
      if (
        sourceLower.includes(projNameLower) ||
        pathLower.includes(projRootLower) ||
        pathLower.includes(`/${projNameLower}/`) ||
        pathLower.includes(`\\${projNameLower}\\`)
      ) {
        edges.push({ a: `m:${m.path}`, b: `p:${p.root}` });
      }
    }
  }

  // memory↔memory by shared source label (cap at 3 edges per node).
  const bySource = new Map<string, RecentMemory[]>();
  for (const m of memory) {
    const k = m.source;
    const arr = bySource.get(k) ?? [];
    arr.push(m);
    bySource.set(k, arr);
  }
  for (const [, group] of bySource) {
    for (let j = 0; j < group.length; j += 1) {
      const m = group[j];
      const limit = Math.min(3, group.length - j - 1);
      for (let k = 1; k <= limit; k += 1) {
        const other = group[j + k];
        edges.push({ a: `m:${m.path}`, b: `m:${other.path}` });
      }
    }
  }

  return { nodes, edges };
}

function stepSim(nodes: GNode[], edges: GEdge[], ticks: number) {
  if (nodes.length === 0) return;
  const byId = new Map<string, GNode>();
  for (const n of nodes) byId.set(n.id, n);

  for (let t = 0; t < ticks; t += 1) {
    // Coulomb repulsion (O(n²) — fine for the small node counts here).
    for (let i = 0; i < nodes.length; i += 1) {
      const a = nodes[i];
      for (let j = i + 1; j < nodes.length; j += 1) {
        const b = nodes[j];
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        let dist2 = dx * dx + dy * dy;
        if (dist2 < 1) {
          dx = (Math.random() - 0.5) * 0.5;
          dy = (Math.random() - 0.5) * 0.5;
          dist2 = dx * dx + dy * dy + 0.01;
        }
        const dist = Math.sqrt(dist2);
        const force = REPULSION / dist2;
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        a.vx += fx;
        a.vy += fy;
        b.vx -= fx;
        b.vy -= fy;
      }
    }

    // Spring attraction along edges (Hooke's law).
    for (const e of edges) {
      const a = byId.get(e.a);
      const b = byId.get(e.b);
      if (!a || !b) continue;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const dist = Math.sqrt(dx * dx + dy * dy) || 0.01;
      const stretch = dist - SPRING_LEN;
      const force = SPRING_K * stretch;
      const fx = (dx / dist) * force;
      const fy = (dy / dist) * force;
      a.vx += fx;
      a.vy += fy;
      b.vx -= fx;
      b.vy -= fy;
    }

    // Center gravity + damping + integrate.
    for (const n of nodes) {
      n.vx += (CX - n.x) * GRAVITY;
      n.vy += (CY - n.y) * GRAVITY;
      n.vx *= DAMPING;
      n.vy *= DAMPING;
      n.x += n.vx;
      n.y += n.vy;
    }
  }
}

interface Tooltip {
  x: number;
  y: number;
  label: string;
  meta: string;
}

export function ProjectGraph() {
  const [snap, setSnap] = useState<BrainSnapshot | null>(null);
  const [, setFrame] = useState(0);
  const graphRef = useRef<{ nodes: GNode[]; edges: GEdge[] } | null>(null);
  const [tip, setTip] = useState<Tooltip | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const animRef = useRef<{ until: number; raf: number | null }>({ until: 0, raf: null });
  const setActive = useCortexStore((s) => s.setActiveProject);
  const resume = useCortexStore((s) => s.resumeSession);

  // Snapshot fetch + 30s refresh.
  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const s = await brainSnapshot();
        if (mounted) setSnap(s);
      } catch {
        /* ignore */
      }
    };
    void tick();
    const id = setInterval(tick, 30_000);
    return () => {
      mounted = false;
      clearInterval(id);
    };
  }, []);

  // Rebuild graph + pre-warm whenever the snapshot changes.
  useEffect(() => {
    if (!snap) return;
    const built = buildGraph(snap);
    stepSim(built.nodes, built.edges, 200);
    graphRef.current = built;
    setFrame((f) => f + 1);

    // Run ~3s of live ticks (5 per frame) then idle.
    const start = performance.now();
    animRef.current.until = start + 3000;
    const loop = () => {
      const g = graphRef.current;
      if (!g) return;
      stepSim(g.nodes, g.edges, 5);
      setFrame((f) => f + 1);
      if (performance.now() < animRef.current.until) {
        animRef.current.raf = requestAnimationFrame(loop);
      } else {
        animRef.current.raf = null;
      }
    };
    if (animRef.current.raf !== null) cancelAnimationFrame(animRef.current.raf);
    animRef.current.raf = requestAnimationFrame(loop);

    return () => {
      if (animRef.current.raf !== null) {
        cancelAnimationFrame(animRef.current.raf);
        animRef.current.raf = null;
      }
    };
  }, [snap]);

  async function onClickNode(n: GNode) {
    if (n.ref.kind === "session") {
      try {
        const stored = await loadSessionMessages(n.ref.data.session_id);
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
        resume(n.ref.data.session_id, msgs);
      } catch {
        /* ignore */
      }
      return;
    }
    if (n.ref.kind === "project") {
      const p = n.ref.data;
      try {
        await setActiveProject(p.root);
        setActive({
          root: p.root,
          name: p.name,
          has_git: p.has_git,
          has_claude_md: p.has_claude_md,
          has_runbooks: p.has_runbooks,
          last_modified_ms: p.last_modified_ms,
          group: "Code",
          kind: "code",
          note_path: null,
          subtitle: null,
        });
        const boot = await bootstrapProjectSession(p.root);
        const msgs: Message[] = boot.messages.map((m) => ({
          id: m.id,
          role: (m.role as Message["role"]) || "system",
          agent: m.agent_id ?? undefined,
          content: m.content,
          reasoning: m.reasoning ?? undefined,
          pending: false,
          tools: [],
          runId: m.run_id,
        }));
        resume(boot.session_id, msgs);
      } catch {
        /* ignore */
      }
      return;
    }
    // memory — open the note in the inline editor, matching the
    // memory-row affordance in BrainPanel / MemoryExplorer.
    if (n.ref.kind === "memory") {
      openInEditor(n.ref.data.path);
    }
  }

  const nodeClass = useMemo(
    () => ({
      project: "pg-node-project",
      session: "pg-node-session",
      memory: "pg-node-memory",
    }),
    [],
  );

  if (!snap) {
    return (
      <div className="project-graph-wrap">
        <PanelLoading label="Loading graph" />
      </div>
    );
  }

  const g = graphRef.current;
  const isEmpty = !g || g.nodes.length === 0;

  return (
    <div className="project-graph-wrap" ref={wrapRef}>
      {isEmpty ? (
        <div className="muted" style={{ padding: 16, textAlign: "center" }}>
          Nothing to graph yet. Open a project or start a session and the map
          will fill in.
        </div>
      ) : (
        <svg
          className="project-graph"
          viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
          preserveAspectRatio="xMidYMid meet"
        >
          {g!.edges.map((e, idx) => {
            const a = g!.nodes.find((n) => n.id === e.a);
            const b = g!.nodes.find((n) => n.id === e.b);
            if (!a || !b) return null;
            return (
              <line
                key={`e-${idx}`}
                className="pg-edge"
                x1={a.x}
                y1={a.y}
                x2={b.x}
                y2={b.y}
              />
            );
          })}
          {g!.nodes.map((n) => (
            <g key={n.id}>
              <circle
                className={nodeClass[n.kind]}
                cx={n.x}
                cy={n.y}
                r={n.size}
                onClick={() => void onClickNode(n)}
                onMouseEnter={(ev) => {
                  const rect = wrapRef.current?.getBoundingClientRect();
                  const x = rect ? ev.clientX - rect.left + 12 : ev.clientX;
                  const y = rect ? ev.clientY - rect.top + 12 : ev.clientY;
                  setTip({ x, y, label: n.label, meta: `${n.kind} · ${n.meta}` });
                }}
                onMouseMove={(ev) => {
                  const rect = wrapRef.current?.getBoundingClientRect();
                  const x = rect ? ev.clientX - rect.left + 12 : ev.clientX;
                  const y = rect ? ev.clientY - rect.top + 12 : ev.clientY;
                  setTip((prev) =>
                    prev ? { ...prev, x, y } : { x, y, label: n.label, meta: `${n.kind} · ${n.meta}` },
                  );
                }}
                onMouseLeave={() => setTip(null)}
              />
              <text
                className="pg-label"
                x={n.x + n.size + 3}
                y={n.y + 3}
              >
                {truncate(n.label, 22)}
              </text>
            </g>
          ))}
        </svg>
      )}
      {tip && (
        <div
          className="pg-tooltip"
          style={{ left: tip.x, top: tip.y, maxWidth: 320 }}
        >
          <div style={{ fontWeight: 600 }}>{tip.label}</div>
          <div className="muted" style={{ fontSize: 10.5, marginTop: 2 }}>{tip.meta}</div>
        </div>
      )}
    </div>
  );
}
