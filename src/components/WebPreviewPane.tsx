import { useEffect, useMemo, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";

import {
  listDevServers,
  subscribePreviewServers,
  type DetectedServer,
} from "@/lib/preview";
import { useCortexStore } from "@/state/store";

/**
 * Localhost dev-server preview tab. Subscribes to `preview:servers`, lets
 * the user pick one, and renders it in a sandboxed iframe. A 🔄 button
 * bumps the iframe key to force a reload; ↗ launches the URL in the
 * system browser via the Tauri shell plugin.
 */
export function WebPreviewPane() {
  const previewUrl = useCortexStore((s) => s.previewUrl);
  const setPreviewUrl = useCortexStore((s) => s.setPreviewUrl);

  const [servers, setServers] = useState<DetectedServer[]>([]);
  const [reloadKey, setReloadKey] = useState(0);
  const [loading, setLoading] = useState(true);

  // One-shot sweep so the dropdown is populated immediately, plus an event
  // subscription for live updates afterwards.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;

    (async () => {
      try {
        const initial = await listDevServers();
        if (!cancelled) setServers(initial);
      } catch {
        /* not in Tauri context — leave empty */
      } finally {
        if (!cancelled) setLoading(false);
      }

      try {
        const fn = await subscribePreviewServers((next) => {
          if (!cancelled) setServers(next);
        });
        if (cancelled) fn();
        else unlisten = fn;
      } catch {
        /* event channel unavailable */
      }
    })();

    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // Auto-select the first server when one shows up and nothing's selected.
  useEffect(() => {
    if (!previewUrl && servers.length > 0) {
      setPreviewUrl(servers[0].url);
    }
    // If the selected URL has disappeared from the list, drop it.
    if (previewUrl && !servers.some((s) => s.url === previewUrl)) {
      // Only clear if we actually have *some* knowledge of the list. If
      // the list is empty we keep the URL — the watcher might just be
      // between ticks.
      if (servers.length > 0) setPreviewUrl(null);
    }
  }, [servers, previewUrl, setPreviewUrl]);

  const selected = useMemo(
    () => servers.find((s) => s.url === previewUrl) ?? null,
    [servers, previewUrl],
  );

  const onPick = (url: string) => {
    setPreviewUrl(url || null);
    setReloadKey((k) => k + 1);
  };

  const onReload = () => setReloadKey((k) => k + 1);

  const onOpenExternal = async () => {
    if (!previewUrl) return;
    try {
      await openExternal(previewUrl);
    } catch (e) {
      console.warn("preview: open external failed", e);
    }
  };

  const empty = !loading && servers.length === 0;

  return (
    <div className="web-preview">
      <div className="web-preview-bar">
        <select
          className="web-preview-select"
          value={previewUrl ?? ""}
          onChange={(e) => onPick(e.target.value)}
          disabled={servers.length === 0}
        >
          {servers.length === 0 && <option value="">(no servers)</option>}
          {servers.map((s) => (
            <option key={s.url} value={s.url}>
              {labelFor(s)}
            </option>
          ))}
        </select>
        <button
          className="web-preview-btn"
          onClick={onReload}
          disabled={!previewUrl}
          title="Reload preview"
          aria-label="Reload preview"
        >
          🔄 refresh
        </button>
        <button
          className="web-preview-btn"
          onClick={onOpenExternal}
          disabled={!previewUrl}
          title="Open in default browser"
          aria-label="Open in default browser"
        >
          ↗ open externally
        </button>
      </div>

      <div className="web-preview-body">
        {empty ? (
          <div className="web-preview-empty">
            No local dev server detected. Cortex polls common ports every 3s.
          </div>
        ) : previewUrl ? (
          <iframe
            key={`${previewUrl}#${reloadKey}`}
            src={previewUrl}
            className="web-preview-frame"
            sandbox="allow-same-origin allow-scripts allow-forms"
            title={selected?.title ?? `Preview of ${previewUrl}`}
          />
        ) : (
          <div className="web-preview-empty">Pick a server above to preview.</div>
        )}
      </div>
    </div>
  );
}

function labelFor(s: DetectedServer): string {
  if (s.title && s.title.trim().length > 0) {
    return `${s.port} — ${s.title}`;
  }
  return `${s.port} — ${s.url}`;
}
