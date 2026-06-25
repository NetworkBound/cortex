import { useEffect, useRef } from "react";
import { KEEP_RECENT, shouldCompact } from "@/lib/compressor";
import { performCondense } from "@/lib/condense";
import { contextLimitForModel } from "@/lib/model-limits";
import { pushToast } from "@/lib/toast";
import { useCortexStore, type Message } from "@/state/store";

/**
 * Auto-condense-on-overflow — the remaining Cline "Condense Context" gap.
 *
 * Cline (and Roo Code) automatically condense the conversation once it
 * approaches the model's context window, so a long session keeps working
 * instead of overflowing. Cortex already has the real LLM condenser
 * (`/compact` → {@link performCondense}); this module adds the *automatic*
 * trigger: estimate the live context size, compare it against a configurable
 * fraction of the model's window, and fire the real condense once when it
 * crosses.
 *
 * The estimate is deliberately cheap and dependency-free (≈ chars/4) — it only
 * needs to be good enough to decide *when* to fold, and it tracks the live
 * `messages` array (unlike the cumulative usage tally in the TokenHUD, which
 * only ever grows and so can never tell when the *current* window is under
 * pressure). Folding collapses the older turns into one summary, which shrinks
 * the estimate back below the threshold, so the trigger naturally settles.
 */

/** Rough bytes-per-token for English/code text. Good enough for a threshold
 *  decision; the real tokenizer lives server-side. */
const CHARS_PER_TOKEN = 4;

/**
 * Estimate the live context size (in tokens) of a message list: the sum of
 * every turn's text plus any tool-call previews, divided by an average
 * characters-per-token. Pure and side-effect free.
 */
export function estimateContextTokens(messages: Message[]): number {
  let chars = 0;
  for (const m of messages) {
    chars += m.content.length;
    if (m.reasoning) chars += m.reasoning.length;
    for (const t of m.tools) {
      if (t.preview) chars += t.preview.length;
    }
  }
  return Math.ceil(chars / CHARS_PER_TOKEN);
}

export interface AutoCondenseInputs {
  /** User setting — feature on/off. */
  enabled: boolean;
  /** A condense is already running for this conversation. */
  busy: boolean;
  /** Estimated live context tokens ({@link estimateContextTokens}). */
  tokens: number;
  /** The model's context window in tokens. */
  limit: number;
  /** Fire at/above this percent of `limit`. */
  thresholdPct: number;
  /** Total turns in the conversation. */
  messageCount: number;
  /** Most-recent turns kept verbatim (never folded). */
  keepRecent: number;
}

/**
 * Pure decision: should auto-condense fire now? Fires only when the feature is
 * enabled, no condense is in flight, there is something to fold (more than
 * `keepRecent` turns), the limit is known, and the estimated context has
 * reached `thresholdPct` of the window.
 */
export function shouldAutoCondense(i: AutoCondenseInputs): boolean {
  if (!i.enabled || i.busy) return false;
  if (i.limit <= 0) return false;
  if (!shouldCompact(i.messageCount, i.keepRecent)) return false;
  const pct = (i.tokens / i.limit) * 100;
  return pct >= i.thresholdPct;
}

/**
 * Mount-once hook that watches the active conversation and auto-condenses it
 * when it crosses the configured threshold. Mounted in `App` so it runs for the
 * whole session regardless of which panel is visible.
 *
 * Re-fire guard: after each auto-condense we remember the (post-fold) message
 * count and refuse to fire again until the conversation has *grown* past it.
 * That makes a runaway loop impossible even if the kept-verbatim window is
 * itself large — we only ever re-evaluate against genuinely new turns.
 */
export function useAutoCondense(): void {
  const messages = useCortexStore((s) => s.messages);
  const enabled = useCortexStore((s) => s.autoCondenseEnabled);
  const thresholdPct = useCortexStore((s) => s.autoCondenseThreshold);
  const model = useCortexStore((s) => s.selectedModel);

  const busyRef = useRef(false);
  // Highest message count at/after which we've already auto-condensed; only
  // grow past this before considering another fold.
  const lastCountRef = useRef(0);

  useEffect(() => {
    if (busyRef.current) return;
    // Only re-evaluate once the conversation has grown since the last auto-fold
    // (prevents re-folding the same summary + kept window in a loop).
    if (messages.length <= lastCountRef.current) return;

    const limit = contextLimitForModel(model);
    const tokens = estimateContextTokens(messages);
    const decision = shouldAutoCondense({
      enabled,
      busy: false,
      tokens,
      limit,
      thresholdPct,
      messageCount: messages.length,
      keepRecent: KEEP_RECENT,
    });
    if (!decision) return;

    busyRef.current = true;
    void performCondense({
      model,
      keepRecent: KEEP_RECENT,
      notify: (title, body, kind) => pushToast({ title, body, kind }),
    })
      .catch(() => {
        // performCondense already degrades to the heuristic + toasts; swallow.
      })
      .finally(() => {
        lastCountRef.current = useCortexStore.getState().messages.length;
        busyRef.current = false;
      });
  }, [messages, enabled, thresholdPct, model]);
}
