import { useEffect, useMemo, useState } from "react";
import {
  groupModelsBySource,
  listModels,
  onModelsChanged,
  sourceMeta,
  type ModelEntry,
} from "@/lib/models";
import { useCortexStore } from "@/state/store";
import "@/styles/model-strip.css";

/**
 * Compact, design-token strip that surfaces every model `list_models`
 * aggregates (Claude CLI + Cortex Gateway catalog + local Ollama), grouped by source.
 *
 * Lives in the bottom status bar. Polls every 45s so dynamically-discovered
 * sources (Ollama coming/going, gateway reconnect) refresh without a reload,
 * and refreshes instantly on backend `models:changed` events (Cookbook pulls).
 * All discovery is best-effort backend-side, so failures just hide the strip
 * rather than surfacing an error — it never blocks the status bar.
 *
 * Pills are controls, not decoration (P0-FINAL Wave 5): clicking an available
 * model sets the store's `selectedModel` (the same per-prompt override the
 * composer's ModelPicker binds), clicking the active pill clears back to
 * auto-routing, and the active model is visually marked. This closes the
 * Cookbook → strip → chat loop without opening the composer picker.
 */
const POLL_MS = 45_000;

export function ModelStrip() {
  const [models, setModels] = useState<ModelEntry[]>([]);
  const selectedModel = useCortexStore((s) => s.selectedModel);
  const setSelectedModel = useCortexStore((s) => s.setSelectedModel);

  useEffect(() => {
    let mounted = true;
    const tick = async () => {
      try {
        const list = await listModels();
        if (mounted) setModels(list);
      } catch {
        /* best-effort: keep last-known list, hide on first failure */
      }
    };
    void tick();
    const id = setInterval(tick, POLL_MS);
    // Refresh immediately when the backend announces a model change (e.g. a
    // Cookbook pull finished) instead of waiting out the 45s poll.
    const unlisten = onModelsChanged(() => void tick());
    return () => {
      mounted = false;
      clearInterval(id);
      void unlisten.then((f) => f());
    };
  }, []);

  const groups = useMemo(() => groupModelsBySource(models), [models]);

  if (groups.length === 0) return null;

  return (
    <div className="model-strip" role="list" aria-label="Available models">
      {groups.map(({ source, models: items }) => {
        const meta = sourceMeta(source);
        const availableCount = items.filter((m) => m.available).length;
        return (
          <div
            key={source}
            className={`model-group source-${meta.hue}`}
            role="listitem"
          >
            <span
              className="model-group-label"
              title={`${meta.label}: ${availableCount}/${items.length} available`}
            >
              {meta.label}
            </span>
            <div className="model-group-models">
              {items.map((m) => {
                const isActive = selectedModel === m.id;
                const title = !m.available
                  ? `${m.label} · ${m.id} · unavailable`
                  : isActive
                    ? `${m.label} · ${m.id} · active chat model — click to return to auto-routing`
                    : `${m.label} · ${m.id} · click to chat with this model`;
                return (
                  <button
                    key={m.id}
                    type="button"
                    className={`model-pill ${m.available ? "available" : "unavailable"}${isActive ? " selected" : ""}`}
                    title={title}
                    aria-pressed={isActive}
                    disabled={!m.available}
                    onClick={() => setSelectedModel(isActive ? null : m.id)}
                  >
                    {m.label}
                  </button>
                );
              })}
            </div>
          </div>
        );
      })}
    </div>
  );
}
