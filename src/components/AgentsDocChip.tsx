import { useEffect, useRef, useState } from "react";
import { agentsMdStack, type AgentsDocSegment } from "@/lib/project-doc";
import { useCortexStore } from "@/state/store";

/**
 * Small chat-header chip that surfaces the merged AGENTS.md stack
 * (Codex / Cursor / Zed convention) for the active project. Shows the
 * segment count when at least one AGENTS.md is found, "—" otherwise.
 * Clicking opens a popover with the full merged view so users can see
 * exactly what Cortex is injecting into the system prompt.
 *
 * Lives only in the chat header — never renders without an active
 * project (there's nothing to anchor the lookup against).
 */
export function AgentsDocChip() {
  const activeProject = useCortexStore((s) => s.activeProject);
  const [segments, setSegments] = useState<AgentsDocSegment[]>([]);
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const popoverRef = useRef<HTMLDivElement | null>(null);

  // Reload the stack whenever the active project changes. The lookup is
  // cheap (a few stat calls + small reads) so we don't bother caching
  // across project switches.
  useEffect(() => {
    let cancelled = false;
    if (!activeProject) {
      setSegments([]);
      return;
    }
    setLoading(true);
    void agentsMdStack(activeProject.root)
      .then((s) => {
        if (!cancelled) setSegments(s);
      })
      .catch(() => {
        // Silent — the chip just shows "—" when the lookup fails, which
        // matches the no-files-found state. We don't want a transient
        // backend hiccup to surface as a scary chat-header error.
        if (!cancelled) setSegments([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [activeProject]);

  // Outside-click / Esc to dismiss — matches SandboxBadge's popover UX so
  // the two chat-header pills feel like the same control surface.
  useEffect(() => {
    if (!open) return;
    function onDocClick(e: MouseEvent) {
      if (!popoverRef.current) return;
      if (!popoverRef.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  if (!activeProject) return null;

  const count = segments.length;
  const has = count > 0;

  return (
    <span className="agents-doc-chip-wrap" ref={popoverRef}>
      <button
        type="button"
        className={`agents-doc-chip${has ? " has-docs" : " no-docs"}`}
        onClick={() => setOpen((v) => !v)}
        title={
          has
            ? `AGENTS.md hierarchy: ${count} file${count === 1 ? "" : "s"} merged into system prompt`
            : "No AGENTS.md found. Drop one in this project root or ~/.cortex/AGENTS.md."
        }
        aria-haspopup="dialog"
        aria-expanded={open}
        disabled={loading}
      >
        <span className="agents-doc-chip-label">AGENTS.md</span>
        <span className="agents-doc-chip-state">
          {loading ? "…" : has ? `✓ ${count}` : "—"}
        </span>
      </button>
      {open && (
        <div
          className="agents-doc-popover"
          role="dialog"
          aria-label="AGENTS.md hierarchical stack"
        >
          <div className="agents-doc-popover-title">
            AGENTS.md hierarchy
            <span className="agents-doc-popover-subtitle">
              merged into the system prompt at session start
            </span>
          </div>
          {segments.length === 0 ? (
            <div className="agents-doc-empty">
              <p>No AGENTS.md files found.</p>
              <p className="agents-doc-empty-hint">
                Cortex looks in:
              </p>
              <ul>
                <li>~/.cortex/AGENTS.md (process-wide)</li>
                <li>~/.codex/AGENTS.md (codex-compat)</li>
                <li>{activeProject.root}/AGENTS.md</li>
                <li>{activeProject.root}/.cortex/AGENTS.md</li>
              </ul>
            </div>
          ) : (
            <ol className="agents-doc-segments">
              {segments.map((seg, i) => (
                <li
                  key={`${seg.scope}-${i}`}
                  className={`agents-doc-segment scope-${seg.scope}`}
                >
                  <div className="agents-doc-segment-head">
                    <span className="agents-doc-segment-scope">
                      {seg.scope}
                    </span>
                    <span
                      className="agents-doc-segment-path"
                      title={seg.path}
                    >
                      {seg.path}
                    </span>
                    <span className="agents-doc-segment-size">
                      {(seg.body.length / 1024).toFixed(1)} KB
                    </span>
                  </div>
                  <pre className="agents-doc-segment-body">{seg.body}</pre>
                </li>
              ))}
            </ol>
          )}
          <div className="agents-doc-popover-foot">
            Later scopes override earlier ones.
          </div>
        </div>
      )}
    </span>
  );
}
