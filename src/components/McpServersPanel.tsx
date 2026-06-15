import { useCallback, useEffect, useRef, useState } from "react";
import { confirmDialog } from "@/lib/dialogs";
import { humanizeError } from "@/lib/errors";
import { createRoot, type Root } from "react-dom/client";
import {
  callMcpTool,
  connectMcp,
  deleteMcpServer,
  disconnectMcp,
  listMcpServers,
  saveMcpServer,
  type McpServerConfig,
  type McpTool,
} from "@/lib/mcp";
import McpCatalog from "@/components/McpCatalog";
import { pushToast } from "@/lib/toast";
import "@/styles/mcp.css";

type McpView = "servers" | "catalog";

interface Props {
  onClose: () => void;
}

/** Split an args string on commas and/or whitespace into clean tokens. */
function parseArgs(raw: string): string[] {
  return raw
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

function McpServersPanel({ onClose }: Props) {
  const [servers, setServers] = useState<McpServerConfig[]>([]);
  const [loading, setLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [tools, setTools] = useState<Record<string, McpTool[]>>({});
  const [view, setView] = useState<McpView>("servers");

  // Add-server form state.
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [argsText, setArgsText] = useState("");
  const [saving, setSaving] = useState(false);
  const nameInputRef = useRef<HTMLInputElement>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setServers(await listMcpServers());
    } catch (e) {
      pushToast({
        title: "Failed to load MCP servers",
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const onAdd = async () => {
    if (!name.trim() || !command.trim()) {
      pushToast({
        title: "Name and command are required",
        kind: "warning",
      });
      return;
    }
    const server: McpServerConfig = {
      id: crypto.randomUUID(),
      name: name.trim(),
      command: command.trim(),
      args: parseArgs(argsText),
      enabled: true,
    };
    setSaving(true);
    try {
      setServers(await saveMcpServer(server));
      pushToast({ title: "Server added", body: server.name, kind: "success" });
      setName("");
      setCommand("");
      setArgsText("");
    } catch (e) {
      pushToast({
        title: "Failed to save server",
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setSaving(false);
    }
  };

  // Jump from the catalog to the manual add form and focus its first field.
  const goManual = useCallback(() => {
    setView("servers");
    // Defer so the form is mounted before we focus it.
    requestAnimationFrame(() => nameInputRef.current?.focus());
  }, []);

  const onConnect = async (s: McpServerConfig) => {
    setBusyId(s.id);
    try {
      const found = await connectMcp(s.id);
      setTools((t) => ({ ...t, [s.id]: found }));
      pushToast({
        title: `Connected to ${s.name}`,
        body: `${found.length} tool${found.length === 1 ? "" : "s"} available`,
        kind: "success",
      });
    } catch (e) {
      pushToast({
        title: `Failed to connect to ${s.name}`,
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setBusyId(null);
    }
  };

  const onDisconnect = async (s: McpServerConfig) => {
    setBusyId(s.id);
    try {
      await disconnectMcp(s.id);
      setTools((t) => {
        const next = { ...t };
        delete next[s.id];
        return next;
      });
      pushToast({ title: `Disconnected from ${s.name}`, kind: "info" });
    } catch (e) {
      pushToast({
        title: `Failed to disconnect from ${s.name}`,
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setBusyId(null);
    }
  };

  const onDelete = async (s: McpServerConfig) => {
    if (!(await confirmDialog({
      title: "Delete MCP server?",
      message: `"${s.name}" will be removed from your configured servers.`,
      confirmLabel: "Delete",
      danger: true,
    }))) return;
    setBusyId(s.id);
    try {
      setServers(await deleteMcpServer(s.id));
      setTools((t) => {
        const next = { ...t };
        delete next[s.id];
        return next;
      });
      pushToast({ title: "Server deleted", body: s.name, kind: "success" });
    } catch (e) {
      pushToast({
        title: "Failed to delete server",
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setBusyId(null);
    }
  };

  const onCallTool = async (s: McpServerConfig, tool: McpTool) => {
    setBusyId(s.id);
    try {
      const result = await callMcpTool(s.id, tool.name);
      pushToast({
        title: `${tool.name} → ok`,
        body: result.length > 280 ? `${result.slice(0, 280)}…` : result,
        kind: "success",
        ttlMs: 7000,
      });
    } catch (e) {
      pushToast({
        title: `${tool.name} failed`,
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal mcp-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="mcp-head">
          <h2>MCP servers</h2>
          <span className="muted">Model Context Protocol connections</span>
        </header>

        <div className="mcp-tabs" role="tablist">
          <button
            role="tab"
            aria-selected={view === "servers"}
            className={`mcp-tab${view === "servers" ? " is-active" : ""}`}
            onClick={() => setView("servers")}
          >
            My servers
            {servers.length > 0 && (
              <span className="mcp-tab-count">{servers.length}</span>
            )}
          </button>
          <button
            role="tab"
            aria-selected={view === "catalog"}
            className={`mcp-tab${view === "catalog" ? " is-active" : ""}`}
            onClick={() => setView("catalog")}
          >
            Browse catalog
          </button>
        </div>

        {view === "catalog" ? (
          <McpCatalog
            servers={servers}
            onAdded={(next) => {
              setServers(next);
              setView("servers");
            }}
            onManual={goManual}
          />
        ) : (
          <>
            <form
              className="mcp-form"
              onSubmit={(e) => {
                e.preventDefault();
                void onAdd();
              }}
            >
              <div className="mcp-form-row">
                <input
                  ref={nameInputRef}
                  className="mcp-input"
                  placeholder="Name (e.g. filesystem)"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  disabled={saving}
                />
                <input
                  className="mcp-input"
                  placeholder="Command (e.g. npx)"
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  disabled={saving}
                />
              </div>
              <input
                className="mcp-input"
                placeholder="Args (comma or space separated)"
                value={argsText}
                onChange={(e) => setArgsText(e.target.value)}
                disabled={saving}
              />
              <div className="mcp-form-actions">
                <button
                  type="button"
                  className="mcp-form-browse"
                  onClick={() => setView("catalog")}
                >
                  Browse catalog
                </button>
                <button className="mcp-primary" type="submit" disabled={saving}>
                  {saving ? "Adding…" : "+ Add server"}
                </button>
              </div>
            </form>

            <div className="mcp-list">
          {loading && servers.length === 0 ? (
            <div className="mcp-empty">Loading…</div>
          ) : servers.length === 0 ? (
            <div className="mcp-empty">
              No MCP servers yet.
              <br />
              <button
                className="mcp-cat-link"
                onClick={() => setView("catalog")}
              >
                Browse the catalog
              </button>{" "}
              to add a well-known server in one click, or fill in the form above
              for a custom one.
            </div>
          ) : (
            servers.map((s) => {
              const connected = tools[s.id] !== undefined;
              const busy = busyId === s.id;
              return (
                <div key={s.id} className="mcp-row">
                  <div className="mcp-row-head">
                    <div className="mcp-row-main">
                      <strong className="mcp-row-name">{s.name}</strong>
                      <code className="mcp-row-cmd">
                        {[s.command, ...s.args].join(" ")}
                      </code>
                    </div>
                    <div className="mcp-row-actions">
                      {connected ? (
                        <button
                          onClick={() => void onDisconnect(s)}
                          disabled={busy}
                        >
                          Disconnect
                        </button>
                      ) : (
                        <button
                          onClick={() => void onConnect(s)}
                          disabled={busy}
                        >
                          {busy ? "…" : "Connect"}
                        </button>
                      )}
                      <button
                        className="mcp-danger"
                        onClick={() => void onDelete(s)}
                        disabled={busy}
                      >
                        Delete
                      </button>
                    </div>
                  </div>
                  {connected && (
                    <div className="mcp-tools">
                      {tools[s.id].length === 0 ? (
                        <div className="muted mcp-tools-empty">
                          No tools advertised by this server.
                        </div>
                      ) : (
                        tools[s.id].map((tool) => (
                          <button
                            key={tool.name}
                            className="mcp-tool"
                            title={tool.description ?? tool.name}
                            onClick={() => void onCallTool(s, tool)}
                            disabled={busy}
                          >
                            <span className="mcp-tool-name">{tool.name}</span>
                            {tool.description && (
                              <span className="mcp-tool-desc">
                                {tool.description}
                              </span>
                            )}
                          </button>
                        ))
                      )}
                    </div>
                  )}
                </div>
              );
            })
          )}
            </div>
          </>
        )}

        <div className="modal-actions">
          <button onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/mcp` slash command (wired by the lead).
 * Same detached-root portal pattern as `openShortcutsModal` (ShortcutsModal.tsx)
 * so the command can pop this panel without any App.tsx wiring.
 */
let activeRoot: Root | null = null;

export function openMcpPanel(): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "mcp";
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
  root.render(<McpServersPanel onClose={close} />);
}
