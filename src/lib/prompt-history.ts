/**
 * Composer prompt history — terminal-style recall of past sends.
 *
 * A small `localStorage`-backed ring of the raw text the user has sent from the
 * chat composer, so Up/Down can cycle through previous prompts (the affordance
 * every terminal-first tool — Aider, the shell, psql — has). Stored oldest→
 * newest; capped so it can't grow unbounded. Pure + client-only; nothing here
 * touches the send pipeline, so it can never affect a message actually going
 * out.
 */

const KEY = "cortex.promptHistory";
const MAX = 50;

/** Load the history, oldest→newest. Returns `[]` on any storage/parse error. */
export function loadPromptHistory(): string[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((x): x is string => typeof x === "string");
  } catch {
    return [];
  }
}

/**
 * Append a sent prompt. Trims; ignores empties and pure slash/`@`-only noise is
 * kept (it's still a real send). Collapses an immediate duplicate of the most
 * recent entry so spamming the same prompt doesn't bloat the ring. Caps at MAX.
 */
export function recordPrompt(text: string): void {
  const trimmed = text.trim();
  if (!trimmed) return;
  try {
    const hist = loadPromptHistory();
    if (hist.length > 0 && hist[hist.length - 1] === trimmed) return;
    hist.push(trimmed);
    const trimmedHist = hist.length > MAX ? hist.slice(hist.length - MAX) : hist;
    localStorage.setItem(KEY, JSON.stringify(trimmedHist));
  } catch {
    /* storage unavailable — history is best-effort */
  }
}
