import { useEffect, useState } from "react";
import { useCortexStore } from "@/state/store";
import { listModels, onModelsChanged, type ModelEntry } from "@/lib/models";
import "@/styles/model-picker.css";

/**
 * Compact per-prompt model selector for the chat composer.
 *
 * Bound to the store's `selectedModel`, which `chatSend` forwards (via the
 * `cortex.selectedModel` localStorage mirror) as a per-call model override.
 * When a Claude model is picked the backend routes the turn to the local
 * `claude` CLI adapter (bypassing the Cortex Gateway); gateway models route to
 * the gateway. The empty-value option ("Auto") clears the override.
 *
 * Model discovery is best-effort: if `listModels()` fails we keep the
 * last-known list (empty on first load → disabled control) rather than
 * throwing. The list refreshes live on backend `models:changed` events.
 */
export function ModelPicker() {
  const [models, setModels] = useState<ModelEntry[]>([]);
  const selectedModel = useCortexStore((s) => s.selectedModel);
  const setSelectedModel = useCortexStore((s) => s.setSelectedModel);

  // Fetch on mount, then refetch whenever the backend announces the model
  // universe changed (e.g. a Cookbook pull landed a new Ollama tag) — the
  // picker stays mounted for the whole session, so without this a freshly
  // pulled model never appears until restart.
  useEffect(() => {
    let alive = true;
    const refresh = () => {
      listModels()
        .then((list) => {
          if (alive) setModels(list);
        })
        .catch(() => {
          /* keep last-known list on transient failure */
        });
    };
    refresh();
    const unlisten = onModelsChanged(refresh);
    return () => {
      alive = false;
      void unlisten.then((f) => f());
    };
  }, []);

  const disabled = models.length === 0;

  // source → optgroup label. Order here is the render order below.
  const SOURCE_LABELS: Record<string, string> = {
    "claude-cli": "Claude (CLI)",
    gateway: "Cortex Gateway",
    ollama: "Ollama (local)",
  };

  const renderOptions = (list: ModelEntry[]) =>
    list.map((m) => (
      <option key={`${m.source}:${m.id}`} value={m.id} disabled={!m.available}>
        {m.label}
      </option>
    ));

  return (
    <div className="model-picker">
      <select
        className="model-picker-select"
        aria-label="Model"
        title="Model for this prompt (overrides the gateway default)"
        value={selectedModel ?? ""}
        disabled={disabled}
        onChange={(e) => setSelectedModel(e.target.value || null)}
      >
        <option value="">Auto (gateway)</option>
        {Object.entries(SOURCE_LABELS).map(([source, label]) => {
          const group = models.filter((m) => m.source === source);
          return group.length > 0 ? (
            <optgroup key={source} label={label}>
              {renderOptions(group)}
            </optgroup>
          ) : null;
        })}
      </select>
    </div>
  );
}

export default ModelPicker;
