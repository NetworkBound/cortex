import { useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import { listHooks, type HooksConfig } from "@/lib/hooks";
import { useCortexStore } from "@/state/store";
import "@/styles/hooks.css";

interface Props {
  onClose: () => void;
}

function HooksPanel({ onClose }: Props) {
  const root = useCortexStore((s) => s.activeProject?.root);
  const [config, setConfig] = useState<HooksConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    if (!root) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    listHooks(root)
      .then((c) => {
        if (!cancelled) setConfig(c);
      })
      .catch((e) => {
        if (!cancelled) setError(humanizeError(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [root]);

  const eventNames = config ? Object.keys(config.events) : [];

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal hooks-panel-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="hooks-panel-head">
          <h2>Hooks</h2>
          <span className="muted">Configured Cortex hooks (read-only)</span>
        </header>

        <div className="hooks-panel-list">
          {!root ? (
            <div className="hooks-panel-empty">
              Open a project first to inspect its hooks.
            </div>
          ) : loading && !config ? (
            <div className="hooks-panel-empty">Loading…</div>
          ) : error ? (
            <div className="hooks-panel-error">
              Failed to load hooks: {error}
            </div>
          ) : eventNames.length === 0 ? (
            <div className="hooks-panel-empty">
              No hooks configured for this project.
            </div>
          ) : (
            eventNames.map((event) => {
              const specs = config!.events[event] ?? [];
              return (
                <section key={event} className="hooks-panel-group">
                  <div className="hooks-panel-group-head">{event}</div>
                  {specs.map((spec, i) => (
                    <div key={i} className="hooks-panel-row">
                      <div className="hooks-panel-row-main">
                        <code className="hooks-panel-row-cmd">
                          {[spec.command, ...spec.args].join(" ")}
                        </code>
                      </div>
                      <span className="hooks-panel-badge">
                        {spec.timeout_ms ?? "default"} ms
                      </span>
                    </div>
                  ))}
                </section>
              );
            })
          )}
        </div>

        <div className="hooks-panel-footer">
          Edit .cortex/hooks/hooks.json to change hooks.
        </div>

        <div className="modal-actions">
          <button onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/hooks` slash command (wired by the lead).
 * Same detached-root portal pattern as `openMcpPanel` (McpServersPanel.tsx)
 * so the command can pop this panel without any App.tsx wiring.
 */
let activeRoot: Root | null = null;

export function openHooksPanel(): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "hooks";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<HooksPanel onClose={close} />);
}
