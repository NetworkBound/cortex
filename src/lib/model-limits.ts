/**
 * Model → context-window lookup for the token HUD.
 *
 * Cortex talks to many providers through the Cortex Gateway, each with a
 * different context budget. Rather than hardcode a single 200k limit we map the
 * model id (or family prefix) to its real window so the usage pill and the
 * compact prompt fire at the right thresholds.
 *
 * Matching is intentionally fuzzy: gateway model ids come in many shapes
 * (`claude-3-7-sonnet-20250219`, `openai/gpt-4o`, `gemini-1.5-pro-latest`,
 * `ollama:llama3.1`, …), so we lowercase and substring-match against known
 * family signatures, longest/most-specific first.
 */

export const DEFAULT_CONTEXT_LIMIT = 200_000;

const K = 1_000;
const M = 1_000_000;

/**
 * Ordered list of [substring, contextWindow] rules. The FIRST rule whose
 * substring appears in the (lowercased) model id wins, so put more specific
 * signatures before broader family names.
 */
const RULES: ReadonlyArray<readonly [string, number]> = [
  // Google Gemini — huge windows, and 1.5/2.x differ.
  ["gemini-1.5-pro", 2 * M],
  ["gemini-1.5", 1 * M],
  ["gemini-2", 1 * M],
  ["gemini", 1 * M],

  // Anthropic Claude — 200k across the modern lineup.
  ["claude", 200 * K],

  // OpenAI — gpt-4.1 / o-series push to 1M & 200k; 4o family is 128k.
  ["gpt-4.1", 1 * M],
  ["o4", 200 * K],
  ["o3", 200 * K],
  ["o1", 200 * K],
  ["gpt-4o", 128 * K],
  ["gpt-4-turbo", 128 * K],
  ["gpt-4", 8 * K],
  ["gpt-3.5", 16 * K],

  // Local / self-hosted — conservative default; many quantized builds run 8k–32k.
  ["ollama", 32 * K],
  ["llama", 32 * K],
  ["mistral", 32 * K],
  ["mixtral", 32 * K],
  ["qwen", 32 * K],
  ["deepseek", 64 * K],
];

/**
 * Best-effort context window for a model id. Falls back to
 * {@link DEFAULT_CONTEXT_LIMIT} (200k) when the model is unknown or absent.
 */
export function contextLimitForModel(model?: string | null): number {
  if (!model) return DEFAULT_CONTEXT_LIMIT;
  const id = model.toLowerCase();
  for (const [needle, limit] of RULES) {
    if (id.includes(needle)) return limit;
  }
  return DEFAULT_CONTEXT_LIMIT;
}
