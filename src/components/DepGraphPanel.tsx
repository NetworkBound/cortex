// DepGraphPanel — SVG force-directed render of the project's import graph.
// Backend (`build_dep_graph`) walks the active project, respects
// `.cortexignore`, parses imports out of every TS/JS/Rust/Python file
// using regex, resolves the relative ones to project-local paths, and
// returns nodes (one per file, sized by line count) + edges (one per
// resolved import). The simulation deliberately mirrors KnowledgeGraph's
// pairwise Coulomb + Hooke physics so the rendering style stays
// consistent across the app — but the code is standalone so each panel
// can evolve independently. Hover highlights the node + its neighbours;
// click dispatches `cortex:editor-open` so the file pops open in the
// editor pane.

import { useEffect, useMemo, useRef, useState } from "react";
import { PanelLoading } from "./Skeleton";
import { humanizeError } from "@/lib/errors";
import {
  buildDepGraph,
  colorForLanguage,
  type DepGraph,
  type DepGraphEdge,
  type DepGraphNode,
} from "@/lib/dep-graph";
import { useCortexStore } from "@/state/store";
import { openInEditor } from "@/lib/editor";

// Simulation tunables — gentle physics so the layout looks readable on
// the first render rather than a tangled hairball. Lifted from
// KnowledgeGraph and kept identical so the visual feel matches.
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

interface SimNode extends DepGraphNode {
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Cached radius (3..18, log-scaled by line count). */
  r: number;
  /** Cached fill colour resolved from `language`. */
  color: string;
}

interface SimGraph {
  nodes: SimNode[];
  edges: DepGraphEdge[];
  /** id → neighbour-id set, for hover dimming. */
  neighbours: Map<string, Set<string>>;
}

function radius(lines: number): number {
  // log-scale so a tiny 10-LOC stub sits at ~4 and a 1500-LOC chonker
  // tops out around 16. Same shape as the KnowledgeGraph helper.
  const v = Math.log10(Math.max(lines, 10));
  return Math.max(3, Math.min(18, 2 + v * 3));
}

function seedPosition(i: number, n: number): { x: number; y: number } {
  const t = (i / Math.max(n, 1)) * Math.PI * 2;
  const r = 200 + ((i * 53) % 140);
  return { x: CX + Math.cos(t) * r, y: CY + Math.sin(t) * r };
}

function makeSimGraph(g: DepGraph, langFilter: Set<string> | null): SimGraph {
  // Apply the language filter at build time so we don't pay the
  // O(n²) simulation cost for nodes the user has hidden.
  const filteredNodes = langFilter
    ? g.nodes.filter((n) => langFilter.has(n.language))
    : g.nodes;
  const idSet = new Set(filteredNodes.map((n) => n.id));
  const n = filteredNodes.length;
  const nodes: SimNode[] = filteredNodes.map((node, idx) => {
    const pos = seedPosition(idx, n);
    return {
      ...node,
      x: pos.x,
      y: pos.y,
      vx: 0,
      vy: 0,
      r: radius(node.lines),
      color: colorForLanguage(node.language),
    };
  });
  const edges = g.edges.filter((e) => idSet.has(e.from) && idSet.has(e.to));
  const neighbours = new Map<string, Set<string>>();
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
    // O(n²) Coulomb repulsion. Capped at 500 nodes server-side so the
    // worst case is ~125k pair iterations per tick — fine on a modern
    // browser.
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
    // Hooke spring attraction along edges.
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
    // Center-gravity + damping + Euler integrate.
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

/** Distinct languages present in the payload, sorted by descending count. */
function languageSummary(g: DepGraph | null): { lang: string; count: number }[] {
  if (!g) return [];
  const tally = new Map<string, number>();
  for (const n of g.nodes) tally.set(n.language, (tally.get(n.language) ?? 0) + 1);
  return Array.from(tally.entries())
    .map(([lang, count]) => ({ lang, count }))
    .sort((a, b) => b.count - a.count);
}

export function DepGraphPanel() {
  const [graph, setGraph] = useState<DepGraph | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [, setFrame] = useState(0);
  const [hoverId, setHoverId] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  // null = no filter (show every language); a Set hides everything outside it.
  const [enabledLangs, setEnabledLangs] = useState<Set<string> | null>(null);
  const simRef = useRef<SimGraph | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const activeProject = useCortexStore((s) => s.activeProject);

  async function load() {
    if (!activeProject?.root) {
      setGraph(null);
      setError("No active project — pick one from the sidebar first.");
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const g = await buildDepGraph(activeProject.root);
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

  // Rebuild + pre-warm the simulation whenever the payload (or the
  // language filter) changes. The filter is applied at sim-build time
  // so hidden nodes don't waste cycles in the O(n²) repulsion loop.
  useEffect(() => {
    if (!graph) {
      simRef.current = null;
      return;
    }
    const sim = makeSimGraph(graph, enabledLangs);
    step(sim, INITIAL_ITERATIONS);
    simRef.current = sim;
    setFrame((f) => f + 1);
  }, [graph, enabledLangs]);

  // Run a short settle pass on container resize so the layout adapts to
  // a wider sidebar or fullscreen toggle without rebuilding the whole sim.
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

  const langs = useMemo(() => languageSummary(graph), [graph]);
  const sim = simRef.current;
  const isEmpty = !sim || sim.nodes.length === 0;

  function toggleLang(lang: string) {
    setEnabledLangs((prev) => {
      const all = new Set(langs.map((l) => l.lang));
      // Starting from "no filter" → first click locks the user into a
      // single chip. From there we toggle membership in the explicit
      // set; emptying it back out turns the filter off entirely.
      const next = new Set(prev ?? all);
      if (next.has(lang)) next.delete(lang);
      else next.add(lang);
      if (next.size === 0 || next.size === all.size) return null;
      return next;
    });
  }

  function chipIsOn(lang: string): boolean {
    if (!enabledLangs) return true;
    return enabledLangs.has(lang);
  }

  return (
    <div className="depgraph-wrap" ref={wrapRef}>
      <div className="depgraph-toolbar">
        <div className="depgraph-chips" role="group" aria-label="Filter by language">
          {langs.map(({ lang, count }) => (
            <button
              key={lang}
              type="button"
              className={`depgraph-chip${chipIsOn(lang) ? " on" : ""}`}
              onClick={() => toggleLang(lang)}
              title={`${count} ${lang} file${count === 1 ? "" : "s"}`}
              style={{ borderColor: colorForLanguage(lang) }}
            >
              <span
                className="depgraph-chip-dot"
                style={{ background: colorForLanguage(lang) }}
              />
              {lang}
              <span className="depgraph-chip-count">{count}</span>
            </button>
          ))}
          {enabledLangs !== null && (
            <button
              type="button"
              className="link-btn"
              onClick={() => setEnabledLangs(null)}
              title="Show every language"
            >
              Clear
            </button>
          )}
        </div>
        <div className="depgraph-zoom-group">
          <button
            type="button"
            className="link-btn"
            onClick={() => setZoom((z) => Math.max(0.4, +(z - 0.2).toFixed(2)))}
            title="Zoom out"
          >
            −
          </button>
          <span className="depgraph-zoom-label">{Math.round(zoom * 100)}%</span>
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
          <span className="depgraph-truncated muted" title="Hit the 500-node / 2000-edge cap">
            truncated
          </span>
        )}
      </div>
      {error ? (
        <div className="muted" style={{ padding: 16 }}>error: {error}</div>
      ) : loading && !sim ? (
        <PanelLoading label="Building dependency graph" />
      ) : isEmpty ? (
        <div className="muted" style={{ padding: 16, textAlign: "center" }}>
          No imports detected — try a project with TypeScript, Rust, or Python files.
        </div>
      ) : (
        <svg
          className="depgraph-svg"
          viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
          preserveAspectRatio="xMidYMid meet"
        >
          <g style={{ transform: `scale(${zoom})`, transformOrigin: `${CX}px ${CY}px` }}>
            {sim!.edges.map((e, idx) => {
              const a = sim!.nodes.find((n) => n.id === e.from);
              const b = sim!.nodes.find((n) => n.id === e.to);
              if (!a || !b) return null;
              const touchesHover =
                hoverId !== null && (hoverId === a.id || hoverId === b.id);
              const dim = hoverId !== null && !touchesHover;
              const cls = `depgraph-edge${touchesHover ? " hover" : ""}${dim ? " dim" : ""}`;
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
              const isHover = hoverId === n.id;
              const isNeighbour =
                hoverId !== null && sim!.neighbours.get(hoverId)?.has(n.id);
              const dim = hoverId !== null && !isHover && !isNeighbour;
              const cls = `depgraph-node${isHover ? " hover" : ""}${dim ? " dim" : ""}`;
              return (
                <g key={n.id}>
                  <circle
                    className={cls}
                    cx={n.x}
                    cy={n.y}
                    r={n.r}
                    fill={n.color}
                    onMouseEnter={() => setHoverId(n.id)}
                    onMouseLeave={() => setHoverId(null)}
                    onClick={() => openOnDisk(activeProject?.root, n.id)}
                  >
                    <title>
                      {`${n.id}\n${n.language} · ${n.lines.toLocaleString("en-US")} line${n.lines === 1 ? "" : "s"}`}
                    </title>
                  </circle>
                  {isHover && (
                    <text
                      className="depgraph-label"
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

/**
 * Open a graph node in the inline editor. Node ids are project-relative
 * forward-slash paths, so we join with the active project root before
 * dispatching the editor event. No-op if there's no active project
 * (defensive — the panel guards `load()` already).
 */
function openOnDisk(root: string | undefined, relId: string): void {
  if (!root) return;
  // The editor accepts an absolute path or a project-rooted relative
  // one; we hand it the absolute form so it doesn't have to guess.
  const sep = root.endsWith("/") ? "" : "/";
  openInEditor(`${root}${sep}${relId}`);
}
