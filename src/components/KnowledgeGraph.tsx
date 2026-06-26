// KnowledgeGraph — SVG force-directed render of the wikilink network across
// every memory source. Backend (`build_knowledge_graph`) walks the same
// markdown files the MemoryExplorer knows about, parses `[[wikilinks]]`,
// and returns nodes (one per file, sized by char count) + edges (one per
// wikilink). The simulation is intentionally tiny and dependency-free: a
// Coulomb-style pairwise repulsion plus Hooke spring attraction along
// edges, 200 iterations on mount and on resize. Hover highlights the
// node + its neighbours; click dispatches `cortex:editor-open`.

import { useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  buildKnowledgeGraph,
  type GraphEdge,
  type GraphNode,
  type KnowledgeGraph as Graph,
} from "@/lib/knowledge-graph";
import { useCortexStore } from "@/state/store";
import { openInEditor } from "@/lib/editor";

// Simulation tunables — gentle physics so the layout looks readable on
// the first render rather than a tangled hairball.
const REPULSION = 1600;
const SPRING_K = 0.015;
const SPRING_LEN = 80;
const GRAVITY = 0.012;
const DAMPING = 0.88;
const INITIAL_ITERATIONS = 200;
const VIEW_W = 1000;
const VIEW_H = 700;
const CX = VIEW_W / 2;
const CY = VIEW_H / 2;

interface SimNode extends GraphNode {
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Cached radius (3..18, log-scaled by char count). */
  r: number;
}

interface SimGraph {
  nodes: SimNode[];
  edges: GraphEdge[];
  /** Pre-built `id → neighbour-id Set` map for hover highlighting. */
  neighbours: Map<string, Set<string>>;
}

function radius(charCount: number): number {
  // log-scale so a tiny note (100 chars) renders at ~4 and a fat one (50k)
  // at ~16 — keeps the smallest visible while preventing megafiles from
  // eating the canvas.
  const v = Math.log10(Math.max(charCount, 10));
  return Math.max(3, Math.min(18, 2 + v * 3));
}

function seedPosition(i: number, n: number): { x: number; y: number } {
  // Loose ring with a slight spiral so the sim has somewhere non-degenerate
  // to start from. The spiral prevents collinear seeds.
  const t = (i / Math.max(n, 1)) * Math.PI * 2;
  const radius = 200 + ((i * 53) % 140);
  return { x: CX + Math.cos(t) * radius, y: CY + Math.sin(t) * radius };
}

function makeSimGraph(g: Graph): SimGraph {
  const n = g.nodes.length;
  const nodes: SimNode[] = g.nodes.map((node, idx) => {
    const pos = seedPosition(idx, n);
    return {
      ...node,
      x: pos.x,
      y: pos.y,
      vx: 0,
      vy: 0,
      r: radius(node.size),
    };
  });
  const neighbours = new Map<string, Set<string>>();
  const idSet = new Set(nodes.map((node) => node.id));
  // Filter edges to nodes we still know about — backend already does
  // this but the second-pass cap can drop a target after its source is
  // already in the list.
  const edges = g.edges.filter((e) => idSet.has(e.from) && idSet.has(e.to));
  for (const e of edges) {
    if (!neighbours.has(e.from)) neighbours.set(e.from, new Set());
    if (!neighbours.has(e.to)) neighbours.set(e.to, new Set());
    neighbours.get(e.from)!.add(e.to);
    neighbours.get(e.to)!.add(e.from);
  }
  return { nodes, edges, neighbours };
}

function step(sim: SimGraph, ticks: number) {
  if (sim.nodes.length === 0) return;
  const byId = new Map<string, SimNode>();
  for (const n of sim.nodes) byId.set(n.id, n);
  for (let t = 0; t < ticks; t += 1) {
    // O(n²) Coulomb repulsion — fine at the 500-node cap.
    for (let i = 0; i < sim.nodes.length; i += 1) {
      const a = sim.nodes[i];
      for (let j = i + 1; j < sim.nodes.length; j += 1) {
        const b = sim.nodes[j];
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
    // Spring attraction along edges.
    for (const e of sim.edges) {
      const a = byId.get(e.from);
      const b = byId.get(e.to);
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
    for (const n of sim.nodes) {
      n.vx += (CX - n.x) * GRAVITY;
      n.vy += (CY - n.y) * GRAVITY;
      n.vx *= DAMPING;
      n.vy *= DAMPING;
      n.x += n.vx;
      n.y += n.vy;
    }
  }
}

function fuzzyMatch(label: string, query: string): boolean {
  if (!query) return true;
  const l = label.toLowerCase();
  const q = query.toLowerCase();
  if (l.includes(q)) return true;
  // Subsequence match — letters of `q` appear in order in `l`.
  let i = 0;
  for (const ch of l) {
    if (ch === q[i]) i += 1;
    if (i >= q.length) return true;
  }
  return false;
}

export function KnowledgeGraph() {
  const [graph, setGraph] = useState<Graph | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [, setFrame] = useState(0);
  const [query, setQuery] = useState("");
  const [hoverId, setHoverId] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  const simRef = useRef<SimGraph | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const activeProject = useCortexStore((s) => s.activeProject);

  async function load() {
    setLoading(true);
    setError(null);
    try {
      const g = await buildKnowledgeGraph(activeProject?.root);
      setGraph(g);
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProject?.root]);

  // Rebuild + pre-warm the simulation whenever the graph payload changes.
  useEffect(() => {
    if (!graph) {
      simRef.current = null;
      return;
    }
    const sim = makeSimGraph(graph);
    step(sim, INITIAL_ITERATIONS);
    simRef.current = sim;
    setFrame((f) => f + 1);
  }, [graph]);

  // Re-run a quick settle pass on container resize so the layout adapts.
  useEffect(() => {
    if (!wrapRef.current) return;
    const ro = new ResizeObserver(() => {
      const sim = simRef.current;
      if (!sim) return;
      step(sim, 40);
      setFrame((f) => f + 1);
    });
    ro.observe(wrapRef.current);
    return () => ro.disconnect();
  }, []);

  const visibleIds = useMemo(() => {
    const sim = simRef.current;
    if (!sim) return new Set<string>();
    if (!query.trim()) return new Set(sim.nodes.map((n) => n.id));
    return new Set(
      sim.nodes
        .filter((n) => fuzzyMatch(n.label, query.trim()))
        .map((n) => n.id),
    );
    // The simulation object is mutated in place, so we only need to
    // recompute when the query changes or the graph is rebuilt.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, graph]);

  const sim = simRef.current;
  const isEmpty = !sim || sim.nodes.length === 0;

  return (
    <div className="kgraph-wrap" ref={wrapRef}>
      <div className="kgraph-toolbar">
        <input
          type="search"
          className="kgraph-search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter nodes…"
          aria-label="Filter knowledge graph nodes by label"
        />
        <div className="kgraph-zoom-group">
          <button
            type="button"
            className="link-btn"
            onClick={() => setZoom((z) => Math.max(0.4, +(z - 0.2).toFixed(2)))}
            title="Zoom out"
          >
            −
          </button>
          <span className="kgraph-zoom-label">{Math.round(zoom * 100)}%</span>
          <button
            type="button"
            className="link-btn"
            onClick={() => setZoom((z) => Math.min(3, +(z + 0.2).toFixed(2)))}
            title="Zoom in"
          >
            +
          </button>
          <button
            type="button"
            className="link-btn"
            onClick={() => setZoom(1)}
            title="Reset zoom"
          >
            ⌂
          </button>
        </div>
        <button
          type="button"
          className="link-btn"
          onClick={() => void load()}
          disabled={loading}
          title="Refresh from disk"
        >
          {loading ? "…" : "Refresh"}
        </button>
        {graph?.truncated && (
          <span className="kgraph-truncated muted" title="Hit the 500-node / 2000-edge cap">
            truncated
          </span>
        )}
      </div>
      {error ? (
        <div className="kgraph-error">{error}</div>
      ) : loading && !sim ? (
        <div className="muted" style={{ padding: 16 }}>building graph…</div>
      ) : isEmpty ? (
        <div className="muted" style={{ padding: 16, textAlign: "center" }}>
          No wikilinks found yet — add some <code>[[links]]</code> across your
          memory entries to see them here.
        </div>
      ) : (
        <svg
          className="kgraph-svg"
          viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
          preserveAspectRatio="xMidYMid meet"
        >
          <g style={{ transform: `scale(${zoom})`, transformOrigin: `${CX}px ${CY}px` }}>
            {sim!.edges.map((e, idx) => {
              const a = sim!.nodes.find((n) => n.id === e.from);
              const b = sim!.nodes.find((n) => n.id === e.to);
              if (!a || !b) return null;
              const visible =
                visibleIds.has(a.id) && visibleIds.has(b.id);
              const touchesHover =
                hoverId !== null && (hoverId === a.id || hoverId === b.id);
              const dim = hoverId !== null && !touchesHover;
              const cls = `kgraph-edge${touchesHover ? " hover" : ""}${dim || !visible ? " dim" : ""}`;
              return (
                <line
                  key={`e-${idx}`}
                  className={cls}
                  x1={a.x}
                  y1={a.y}
                  x2={b.x}
                  y2={b.y}
                />
              );
            })}
            {sim!.nodes.map((n) => {
              const visible = visibleIds.has(n.id);
              const isHover = hoverId === n.id;
              const isNeighbour =
                hoverId !== null && sim!.neighbours.get(hoverId)?.has(n.id);
              const dim = hoverId !== null && !isHover && !isNeighbour;
              const cls = `kgraph-node${isHover ? " hover" : ""}${dim || !visible ? " dim" : ""}`;
              return (
                <g key={n.id}>
                  <circle
                    className={cls}
                    cx={n.x}
                    cy={n.y}
                    r={n.r}
                    onMouseEnter={() => setHoverId(n.id)}
                    onMouseLeave={() => setHoverId(null)}
                    onClick={() => openInEditor(n.path)}
                  >
                    <title>{`${n.label}\n${n.source}\n${n.size.toLocaleString("en-US")} chars`}</title>
                  </circle>
                  {(isHover || query.trim().length > 0) && visible && (
                    <text
                      className="kgraph-label"
                      x={n.x + n.r + 3}
                      y={n.y + 3}
                    >
                      {n.label.length > 28 ? n.label.slice(0, 27) + "…" : n.label}
                    </text>
                  )}
                </g>
              );
            })}
          </g>
        </svg>
      )}
    </div>
  );
}
