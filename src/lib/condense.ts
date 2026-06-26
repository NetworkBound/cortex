import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";
import { buildSummaryMessage } from "@/lib/compressor";
import { useCortexStore, type Message } from "@/state/store";

/** A single conversation turn handed to the backend condenser. */
export interface CondenseTurn {
  role: string;
  content: string;
}

/** Outcome of `condense_history` — the model-written summary of the folded
 *  turns plus how many turns it covered and which model produced it. */
export interface CondenseResult {
  summary: string;
  folded: number;
  model: string;
}

/**
 * Ask the backend to condense `turns` (the older portion of the conversation)
 * into a single faithful, structured summary using `model` (the currently
 * selected model; null lets the router pick its default). Rejects on an empty
 * input, no available model, a timeout, or an empty completion — the caller is
 * expected to fall back to the cheap heuristic summary so `/compact` never
 * hard-fails.
 */
export async function condenseHistory(
  turns: CondenseTurn[],
  model: string | null,
): Promise<CondenseResult> {
  return invoke<CondenseResult>("condense_history", { turns, model });
}

/** Notify sink shape shared by `/compact`, the TokenHUD button, and the
 *  auto-condense hook (a subset of the slash/toast signatures). */
export type CondenseNotify = (
  title: string,
  body: string,
  kind: "info" | "success" | "warning",
) => void;

export interface PerformCondenseOptions {
  /** Selected model id, or null to let the router pick its default. */
  model: string | null;
  /** How many most-recent turns to keep verbatim (older turns are folded). */
  keepRecent: number;
  /** Progress / outcome sink. */
  notify: CondenseNotify;
}

/**
 * The single condense runner behind `/compact`, the TokenHUD "Compact" button,
 * and auto-condense-on-overflow. Folds everything before the most-recent
 * `keepRecent` turns into ONE summary system message:
 *
 *   1. Slice off the older turns (everything except the last `keepRecent`).
 *   2. Ask the selected model for a faithful structured summary
 *      ({@link condenseHistory}); on any failure (offline / timeout / no model)
 *      fall back to the cheap heuristic {@link buildSummaryMessage} so a
 *      condense never loses the user their compaction.
 *   3. Re-read the live messages (a reply may have streamed in during the model
 *      call) and adopt `[summary, ...lastKeepRecent]` onto the active thread via
 *      `adoptSession` (keeps the thread record + legacy mirrors in lock-step).
 *
 * Returns `true` when a condense ran, `false` when there was nothing to fold
 * (≤ `keepRecent` turns) — the caller decides whether to surface a skip.
 */
export async function performCondense(opts: PerformCondenseOptions): Promise<boolean> {
  const store = useCortexStore;
  const messages = store.getState().messages;
  const cutoff = messages.length - opts.keepRecent;
  if (cutoff <= 0) return false;
  const older = messages.slice(0, cutoff);
  opts.notify("Condensing…", `Summarizing ${older.length} earlier turns.`, "info");

  let summary: Message;
  try {
    // Real condense: ask the selected model for a faithful, structured summary
    // of the older turns (Cline "Condense Context" behaviour).
    const res = await condenseHistory(
      older.map((m) => ({ role: m.role, content: m.content })),
      opts.model,
    );
    summary = {
      id: `compact-${crypto.randomUUID()}`,
      role: "system",
      content: `📚 **Condensed ${res.folded} earlier turns** (via ${res.model})\n\n${res.summary}`,
      tools: [],
    };
    opts.notify("Conversation condensed", `${res.folded} turns folded into a summary.`, "success");
  } catch (e) {
    // Graceful degrade: the model was unavailable / timed out — keep the cheap
    // heuristic summary so a condense never loses the user their compaction.
    summary = buildSummaryMessage(older);
    opts.notify(
      "Condensed (offline fallback)",
      `Used a heuristic summary — model condense unavailable: ${humanizeError(e)}`,
      "warning",
    );
  }

  // Re-read in case a reply streamed in during the model call; always keep the
  // most-recent window verbatim and fold everything before it.
  const latest = store.getState().messages;
  const keep = latest.slice(Math.max(0, latest.length - opts.keepRecent));
  store.getState().adoptSession({ messages: [summary, ...keep] });
  return true;
}
