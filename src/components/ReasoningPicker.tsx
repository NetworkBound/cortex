import { useCortexStore } from "@/state/store";
import "@/styles/model-picker.css";

/**
 * Compact per-prompt reasoning-effort selector for the chat composer (OpenAI
 * Codex CLI parity — `model_reasoning_effort`).
 *
 * Bound to the store's `selectedReasoningEffort`, which `chatSend` forwards (via
 * the `cortex.selectedReasoningEffort` localStorage mirror) as a per-call
 * override. The backend (`orchestrator::reasoning::resolve`) lets a valid pick
 * win over the global config default and drops anything unrecognized, so an
 * empty pick ("Auto") cleanly falls back to that default.
 *
 * Only adapters that can act on it (the gateway → reasoning upstreams) consume the
 * value; the rest ignore it — picking a level never breaks a non-reasoning model.
 */
const LEVELS: ReadonlyArray<{ value: string; label: string }> = [
  { value: "minimal", label: "Minimal" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
];

export function ReasoningPicker() {
  const selected = useCortexStore((s) => s.selectedReasoningEffort);
  const setSelected = useCortexStore((s) => s.setSelectedReasoningEffort);

  return (
    <div className="model-picker">
      <select
        className="model-picker-select"
        aria-label="Reasoning effort"
        title="Reasoning effort for this prompt (overrides the default)"
        value={selected ?? ""}
        onChange={(e) => setSelected(e.target.value || null)}
      >
        <option value="">Reasoning: Auto</option>
        {LEVELS.map((l) => (
          <option key={l.value} value={l.value}>
            Reasoning: {l.label}
          </option>
        ))}
      </select>
    </div>
  );
}

export default ReasoningPicker;
