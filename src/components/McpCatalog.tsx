import { useMemo, useState } from "react";
import { saveMcpServer, type McpServerConfig } from "@/lib/mcp";
import {
  CATEGORY_LABELS,
  isEntryAdded,
  matchesQuery,
  MCP_CATALOG,
  resolveArgs,
  type CatalogEntry,
} from "@/lib/mcp-catalog";
import { pushToast } from "@/lib/toast";
import { humanizeError } from "@/lib/errors";

interface Props {
  /** Currently configured servers (for "already added" state). */
  servers: McpServerConfig[];
  /** Called with the refreshed list after a successful add. */
  onAdded: (servers: McpServerConfig[]) => void;
  /** Switch to the manual/custom add form. */
  onManual: () => void;
}

const ALL_CATEGORIES = Object.keys(CATEGORY_LABELS) as CatalogEntry["category"][];

/**
 * Curated catalog of well-known MCP servers with one-click add. Entries that
 * need env vars or a path open an inline config card so the user supplies the
 * values (we never hardcode tokens); the rest add immediately.
 */
function McpCatalog({ servers, onAdded, onManual }: Props) {
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<CatalogEntry["category"] | "all">(
    "all",
  );
  // The entry currently being configured (env / path prompts), if any.
  const [configuring, setConfiguring] = useState<CatalogEntry | null>(null);
  const [fills, setFills] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);

  const results = useMemo(
    () =>
      MCP_CATALOG.filter(
        (e) =>
          (category === "all" || e.category === category) &&
          matchesQuery(e, query),
      ),
    [query, category],
  );

  const needsConfig = (e: CatalogEntry) =>
    (e.env?.length ?? 0) > 0 || (e.argPrompts?.length ?? 0) > 0;

  const persist = async (entry: CatalogEntry, filled: Record<string, string>) => {
    const env: Record<string, string> = {};
    for (const v of entry.env ?? []) {
      const value = filled[v.key]?.trim();
      if (value) env[v.key] = value;
    }
    const server: McpServerConfig = {
      id: crypto.randomUUID(),
      name: entry.name,
      command: entry.command,
      args: resolveArgs(entry, filled),
      enabled: true,
      ...(Object.keys(env).length > 0 ? { env } : {}),
    };
    setSaving(true);
    try {
      const next = await saveMcpServer(server);
      onAdded(next);
      pushToast({ title: "Added from catalog", body: entry.name, kind: "success" });
      setConfiguring(null);
      setFills({});
    } catch (e) {
      pushToast({
        title: "Failed to add server",
        body: humanizeError(e),
        kind: "error",
      });
    } finally {
      setSaving(false);
    }
  };

  const onAdd = (entry: CatalogEntry) => {
    if (needsConfig(entry)) {
      setFills({});
      setConfiguring(entry);
      return;
    }
    void persist(entry, {});
  };

  // Validation for the inline config card: every required env var + every
  // arg prompt must be filled before we let the user add.
  const configReady = useMemo(() => {
    if (!configuring) return false;
    const envOk = (configuring.env ?? []).every(
      (v) => (fills[v.key]?.trim().length ?? 0) > 0,
    );
    const argsOk = (configuring.argPrompts ?? []).every(
      (p) => (fills[p.token]?.trim().length ?? 0) > 0,
    );
    return envOk && argsOk;
  }, [configuring, fills]);

  if (configuring) {
    const entry = configuring;
    return (
      <div className="mcp-cat-config">
        <div className="mcp-cat-config-head">
          <button
            className="mcp-cat-back"
            onClick={() => {
              setConfiguring(null);
              setFills({});
            }}
            disabled={saving}
          >
            ‹ Back
          </button>
          <div className="mcp-cat-config-title">
            <strong>{entry.name}</strong>
            <span className="muted">{entry.description}</span>
          </div>
        </div>

        {entry.argPrompts?.map((p) => (
          <label key={p.token} className="mcp-cat-field">
            <span className="mcp-cat-field-label">{p.label}</span>
            <input
              className="mcp-input"
              placeholder={p.placeholder}
              value={fills[p.token] ?? ""}
              onChange={(e) =>
                setFills((f) => ({ ...f, [p.token]: e.target.value }))
              }
              disabled={saving}
              autoFocus
            />
            {p.hint && <span className="mcp-cat-field-hint">{p.hint}</span>}
          </label>
        ))}

        {entry.env?.map((v) => (
          <label key={v.key} className="mcp-cat-field">
            <span className="mcp-cat-field-label">
              {v.label} <code className="mcp-cat-envkey">{v.key}</code>
            </span>
            <input
              className="mcp-input"
              type={v.secret === false ? "text" : "password"}
              placeholder={v.placeholder}
              value={fills[v.key] ?? ""}
              onChange={(e) =>
                setFills((f) => ({ ...f, [v.key]: e.target.value }))
              }
              disabled={saving}
              autoComplete="off"
            />
            {v.hint && <span className="mcp-cat-field-hint">{v.hint}</span>}
          </label>
        ))}

        <div className="mcp-cat-config-actions">
          {entry.homepage && (
            <a
              className="mcp-cat-link"
              href={entry.homepage}
              target="_blank"
              rel="noreferrer"
            >
              Learn more
            </a>
          )}
          <button
            className="mcp-primary"
            onClick={() => void persist(entry, fills)}
            disabled={saving || !configReady}
          >
            {saving ? "Adding…" : "Add server"}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="mcp-cat">
      <div className="mcp-cat-controls">
        <input
          className="mcp-input"
          placeholder="Search the catalog (e.g. github, browser, sql)…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
        />
        <div className="mcp-cat-cats">
          <button
            className={`mcp-cat-chip${category === "all" ? " is-active" : ""}`}
            onClick={() => setCategory("all")}
          >
            All
          </button>
          {ALL_CATEGORIES.map((c) => (
            <button
              key={c}
              className={`mcp-cat-chip${category === c ? " is-active" : ""}`}
              onClick={() => setCategory(c)}
            >
              {CATEGORY_LABELS[c]}
            </button>
          ))}
        </div>
      </div>

      <div className="mcp-cat-grid">
        {results.length === 0 ? (
          <div className="mcp-empty">
            Nothing matches “{query.trim()}”.
            <br />
            <button className="mcp-cat-link" onClick={onManual}>
              Add a custom server instead
            </button>
          </div>
        ) : (
          results.map((entry) => {
            const added = isEntryAdded(entry, servers);
            return (
              <div key={entry.id} className="mcp-cat-card">
                <div className="mcp-cat-card-head">
                  <strong className="mcp-cat-card-name">{entry.name}</strong>
                  <span className="mcp-cat-card-cat">
                    {CATEGORY_LABELS[entry.category]}
                  </span>
                </div>
                <p className="mcp-cat-card-desc">{entry.description}</p>
                <div className="mcp-cat-card-foot">
                  <code className="mcp-cat-card-cmd">
                    {[entry.command, ...entry.argsTemplate].join(" ")}
                  </code>
                  {added ? (
                    <span className="mcp-cat-added" title="Already in your list">
                      ✓ Added
                    </span>
                  ) : (
                    <button
                      className="mcp-cat-add"
                      onClick={() => onAdd(entry)}
                    >
                      {needsConfig(entry) ? "Configure…" : "+ Add"}
                    </button>
                  )}
                </div>
              </div>
            );
          })
        )}
      </div>

      <div className="mcp-cat-manual">
        Don’t see what you need?{" "}
        <button className="mcp-cat-link" onClick={onManual}>
          Add a custom server
        </button>
      </div>
    </div>
  );
}

export default McpCatalog;
