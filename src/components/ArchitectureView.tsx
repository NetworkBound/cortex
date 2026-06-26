import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { PanelLoading } from "./Skeleton";
import { humanizeError } from "@/lib/errors";
import { invoke } from "@tauri-apps/api/core";
import { useCortexStore } from "@/state/store";
import "../styles/architecture.css";

// ---------------------------------------------------------------------------
// Shared "architecture tab open" flag.
//
// The architecture panel is a NEW activity surface, but the `ActivityTab`
// union (state/store.ts) and ACTIVITY_ICONS (lib/activity-icons.tsx) are owned
// by other modules this wave, so we cannot extend them. Instead this tiny
// external store lets ActivityBar (the pill) and ActivityPanel (the render
// branch) coordinate the open/closed state independently of the global
// `activityTab`. Opening the arch tab also clears the global tab so only one
// panel shows at a time.
// ---------------------------------------------------------------------------

let archOpen = false;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

export const archTab = {
  isOpen: () => archOpen,
  open() {
    if (archOpen) return;
    archOpen = true;
    // Close any global activity tab so the panels don't stack.
    useCortexStore.getState().setActivityTab(null);
    emit();
  },
  close() {
    if (!archOpen) return;
    archOpen = false;
    emit();
  },
  toggle() {
    if (archOpen) this.close();
    else this.open();
  },
  subscribe(fn: () => void) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  },
};

/** React hook: subscribe to the architecture-tab open flag. */
export function useArchTabOpen(): boolean {
  return useSyncExternalStore(
    (cb) => archTab.subscribe(cb),
    () => archOpen,
    () => archOpen,
  );
}

// ---------------------------------------------------------------------------

interface ArchDiagram {
  mermaid: string;
  description: string;
  sha: string;
  cached: boolean;
}

type Phase = "idle" | "loading" | "ready" | "error";

export function ArchitectureView() {
  const projectRoot = useCortexStore((s) => s.activeProject?.root ?? null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [data, setData] = useState<ArchDiagram | null>(null);
  const [error, setError] = useState<string>("");
  const [svg, setSvg] = useState<string>("");
  const renderSeq = useRef(0);

  const generate = useCallback(
    async (force: boolean) => {
      if (!projectRoot) {
        setPhase("error");
        setError("Open a project first to map its architecture.");
        return;
      }
      setPhase("loading");
      setError("");
      try {
        const result = await invoke<ArchDiagram>("generate_arch_diagram", {
          projectRoot,
          force,
        });
        setData(result);
        await renderMermaid(result.mermaid);
        setPhase("ready");
      } catch (e) {
        setError(humanizeError(e));
        setData(null);
        setSvg("");
        setPhase("error");
      }
    },
    // renderMermaid is stable (defined below via closure over refs/setters).
     
    [projectRoot],
  );

  // Render the Mermaid source to SVG via a LAZY dynamic import so the heavy
  // mermaid bundle is code-split out of the main chunk.
  async function renderMermaid(definition: string) {
    const seq = ++renderSeq.current;
    try {
      const mermaid = (await import("mermaid")).default;
      mermaid.initialize({
        startOnLoad: false,
        theme: "dark",
        securityLevel: "strict",
      });
      const id = `arch-mermaid-${seq}`;
      const { svg: out } = await mermaid.render(id, definition);
      // Drop stale renders if a newer regenerate landed first.
      if (seq === renderSeq.current) setSvg(out);
    } catch (e) {
      if (seq === renderSeq.current) {
        setSvg("");
        setError(`Diagram render failed: ${humanizeError(e)}`);
        setPhase("error");
      }
    }
  }

  // Auto-generate on first mount / when the active project changes.
  useEffect(() => {
    if (projectRoot) void generate(false);
    else {
      setPhase("idle");
      setData(null);
      setSvg("");
    }
  }, [projectRoot, generate]);

  return (
    <div className="arch-view">
      <div className="arch-toolbar">
        <span className="arch-title">Architecture</span>
        {data?.sha && (
          <span className="arch-sha" title="git SHA this diagram maps">
            {data.sha === "nogit" ? "(no git)" : data.sha.slice(0, 8)}
            {data.cached ? " · cached" : ""}
          </span>
        )}
        <button
          className="arch-btn"
          disabled={phase === "loading" || !projectRoot}
          onClick={() => void generate(true)}
          title="Re-walk the tree and regenerate (bypasses cache)"
        >
          {phase === "loading" ? "Generating…" : "Regenerate"}
        </button>
      </div>

      <div className="arch-body">
        {phase === "idle" && (
          <div className="arch-state">Open a project to map its architecture.</div>
        )}
        {phase === "loading" && (
          <PanelLoading label="Walking the tree and synthesising the diagram" />
        )}
        {phase === "error" && <div className="arch-error">{error}</div>}
        {phase === "ready" && data && (
          <>
            {data.description && <p className="arch-description">{data.description}</p>}
            {svg ? (
              <div className="arch-diagram" dangerouslySetInnerHTML={{ __html: svg }} />
            ) : (
              <div className="arch-state">Rendering diagram…</div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
