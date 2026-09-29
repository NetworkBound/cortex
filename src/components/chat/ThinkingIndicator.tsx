import { useCortexStore } from "@/state/store";

const AGENT_LABELS: Record<string, string> = {
  "codex-cli": "Codex",
  "claude-cli": "Claude",
  "gemini-cli": "Gemini",
  "qwen-cli": "Qwen",
  "grok-cli": "Grok",
  "aider-cli": "Aider",
  "mistral-cli": "Mistral",
  "gateway-remote": "Gateway",
  ollama: "Ollama",
};

/**
 * Three pulsing dots + "<model> is thinking" shown in an assistant bubble
 * that has started streaming but produced no token, tool or reasoning yet.
 */
export function ThinkingIndicator({ agent }: { agent?: string | null }) {
  const selectedModel = useCortexStore((s) => s.selectedModel);
  const label = selectedModel
    ? selectedModel.toUpperCase().replace(/-/g, " ")
    : agent
      ? (AGENT_LABELS[agent] ?? agent)
      : "Agent";
  return (
    <div className="msg-thinking" role="status" aria-live="polite">
      <span className="thinking-dot" />
      <span className="thinking-dot" />
      <span className="thinking-dot" />
      <span className="thinking-label">{label} is thinking</span>
    </div>
  );
}
