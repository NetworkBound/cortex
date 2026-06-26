/**
 * Natural-language slash router.
 *
 * `/ask <query>` calls the backend `ask_router` command, which prompts the gateway
 * to map the user's free-form text onto the closest existing slash command.
 * This module owns the IPC wrapper plus the dispatch logic that takes the
 * model's verdict and either:
 *   - high-confidence (≥ HIGH_CONFIDENCE): runs the slash immediately;
 *   - low-confidence (< HIGH_CONFIDENCE, > 0): shows a confirm toast + posts
 *     a click-to-run system note ("Did you mean `/cmd args`?");
 *   - no match: posts a system note with the model's reason.
 *
 * We deliberately resolve the matched name through `findCommand` so custom
 * user-defined slashes (loaded into the registry at boot) work the same way
 * as built-ins.
 */
import { invoke } from "@tauri-apps/api/core";

import {
  COMMANDS,
  findCommand,
  makeContext,
  type SlashCommand,
  type SlashContext,
} from "@/lib/slash-commands";
import { pushToast } from "@/lib/toast";
import { humanizeError } from "@/lib/errors";

/** Mirror of `SlashSpec` in `src-tauri::commands::ask_router`. */
export interface SlashSpec {
  name: string;
  description: string;
  aliases: string[];
  usage: string | null;
}

/** Mirror of `AskResult` in `src-tauri::commands::ask_router`. */
export interface AskResult {
  matched_slash: string | null;
  suggested_args: string;
  confidence: number;
  reason: string;
}

/** Above this, we run the slash without asking. Tuned by feel — the prompt
 *  asks the model to use < 0.5 for "no match", so 0.75 gives a comfortable
 *  middle band for "did you mean…" confirmation. */
const HIGH_CONFIDENCE = 0.75;

/** Snapshot the live slash registry into the shape the backend expects.
 *  Re-evaluated on every `/ask` so custom slashes added at runtime are
 *  visible to the router. */
export function collectSlashSpecs(): SlashSpec[] {
  const seen = new Set<SlashCommand>();
  const out: SlashSpec[] = [];
  for (const c of COMMANDS) {
    if (seen.has(c)) continue;
    seen.add(c);
    out.push({
      name: c.name,
      description: c.description,
      aliases: c.aliases ? [...c.aliases] : [],
      usage: c.usage ?? null,
    });
  }
  return out;
}

/** Thin IPC wrapper. Throws on backend errors so callers can surface the
 *  rejection verbatim. */
export async function askRouter(
  query: string,
  availableSlashes?: SlashSpec[],
): Promise<AskResult> {
  const slashes = availableSlashes ?? collectSlashSpecs();
  return invoke<AskResult>("ask_router", {
    query,
    availableSlashes: slashes,
  });
}

/** Format a slash + args for display (e.g. "/changelog 1d" or "/cost"). */
function formatInvocation(name: string, args: string): string {
  const a = args.trim();
  return a ? `/${name} ${a}` : `/${name}`;
}

/** Build the chat input string the slash dispatcher expects. */
function buildSlashInput(name: string, args: string): string {
  const a = args.trim();
  return a ? `/${name} ${a}` : `/${name}`;
}

/**
 * Execute the `/ask` flow end-to-end. Caller owns the slash-context — we
 * default to `makeContext()` so the command can be invoked from anywhere.
 */
export async function runAsk(
  query: string,
  ctx: SlashContext = makeContext(),
): Promise<void> {
  const trimmed = query.trim();
  if (!trimmed) {
    ctx.notify(
      "/ask",
      "Usage: /ask <natural language question>. Example: /ask show me what changed today.",
      "warning",
    );
    return;
  }

  let result: AskResult;
  try {
    result = await askRouter(trimmed);
  } catch (e) {
    ctx.append({
      id: `e-${crypto.randomUUID()}`,
      role: "error",
      content: `/ask failed: ${humanizeError(e)}`,
      tools: [],
    });
    return;
  }

  await dispatchAskResult(result, ctx);
}

/**
 * Take a router verdict and act on it. Exported so future surfaces (e.g. an
 * omnibar natural-language mode) can call it without re-running the model.
 */
export async function dispatchAskResult(
  result: AskResult,
  ctx: SlashContext = makeContext(),
): Promise<void> {
  const { matched_slash, suggested_args, confidence, reason } = result;

  if (!matched_slash) {
    ctx.append({
      id: `s-${crypto.randomUUID()}`,
      role: "system",
      content: `/ask: no matching slash command. ${reason}`,
      tools: [],
    });
    return;
  }

  const invocation = formatInvocation(matched_slash, suggested_args);
  const input = buildSlashInput(matched_slash, suggested_args);
  const command = findCommand(input);

  if (!command) {
    // Backend already resolves aliases, but the registry can shift between
    // the snapshot and dispatch. Degrade to a system note rather than a
    // silent failure.
    ctx.append({
      id: `s-${crypto.randomUUID()}`,
      role: "system",
      content: `/ask suggested ${invocation}, but that command is no longer registered.`,
      tools: [],
    });
    return;
  }

  if (confidence >= HIGH_CONFIDENCE) {
    ctx.notify("/ask", `Running ${invocation} — ${reason}`, "info");
    try {
      await command.run(suggested_args, ctx);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/ask: running ${invocation} failed: ${humanizeError(e)}`,
        tools: [],
      });
    }
    return;
  }

  // Low-confidence: ask before running. Toast is the cheap surface; the
  // system note is the click-to-run fallback for users who miss the toast.
  pushToast({
    title: "/ask — did you mean…",
    body: `${invocation} (confidence ${(confidence * 100).toFixed(0)}%). Run \`${invocation}\` to confirm.`,
    kind: "info",
    ttlMs: 8000,
  });
  ctx.append({
    id: `s-${crypto.randomUUID()}`,
    role: "system",
    content: `/ask: did you mean \`${invocation}\`? (${reason}) — type it to run.`,
    tools: [],
  });
}
