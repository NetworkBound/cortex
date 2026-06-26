// User-defined slash commands — loaded at app boot from
// `~/.cortex/custom-slashes.yaml` (via the `list_custom_slashes` Tauri
// command) and grafted onto the existing `COMMANDS` array in
// `slash-commands.ts`.
//
// Each saved entry has a `body` field interpreted as one slash command per
// line. When the user runs `/<name>`, `runCustomBody(body, ctx)` splits the
// body, looks each line up via `findCommand`, and dispatches them
// sequentially — sharing the same `SlashContext` so toasts / appends /
// store mutations all land in the live chat.
//
// Discovery is fire-and-forget at module init time (see the tail of
// `slash-commands.ts`). That means a custom slash typed immediately after
// app launch may miss the first lookup — acceptable for v1, and the user
// can always re-open the chat or refire the slash.

import { invoke } from "@tauri-apps/api/core";
import { humanizeError } from "@/lib/errors";

import {
  COMMANDS,
  findCommand,
  parseInput,
  rebuildSlashIndex,
  type SlashCommand,
  type SlashContext,
} from "@/lib/slash-commands";

export interface CustomSlash {
  name: string;
  description: string;
  body: string;
}

const CUSTOM_TAG = Symbol.for("cortex.customSlash");

interface TaggedCommand extends SlashCommand {
  [CUSTOM_TAG]?: true;
}

function isCustom(cmd: SlashCommand): cmd is TaggedCommand {
  return (cmd as TaggedCommand)[CUSTOM_TAG] === true;
}

/** Fetch every saved custom slash. Returns `[]` on any backend failure so
 *  callers can fall through to the built-in command set without branching. */
export async function loadCustomSlashes(): Promise<CustomSlash[]> {
  try {
    const out = await invoke<CustomSlash[]>("list_custom_slashes");
    return Array.isArray(out) ? out : [];
  } catch (err) {
    console.warn("loadCustomSlashes failed", err);
    return [];
  }
}

/** Persist (upsert) a single custom slash. Resolves to the row as saved, or
 *  `null` when the backend rejects the payload. */
export async function saveCustomSlash(
  slash: CustomSlash,
): Promise<CustomSlash | null> {
  try {
    return await invoke<CustomSlash>("save_custom_slash", { slash });
  } catch (err) {
    console.warn("saveCustomSlash failed", err);
    return null;
  }
}

/** Delete by name. Missing entries are a backend no-op. */
export async function deleteCustomSlash(name: string): Promise<boolean> {
  try {
    await invoke("delete_custom_slash", { name });
    return true;
  } catch (err) {
    console.warn("deleteCustomSlash failed", err);
    return false;
  }
}

/**
 * Run a multi-line custom body. Each non-blank line is parsed as its own
 * slash command and dispatched via the shared `SlashContext`. Unknown
 * commands surface as toasts so the user can tell why a step was skipped.
 *
 * Steps run sequentially (`await`-ed) so cascading effects — e.g.
 * `/stage` → `/commit-msg` — see the previous step's state. We swallow
 * per-step errors so one bad line doesn't abort the rest of the chain.
 */
export async function runCustomBody(
  body: string,
  ctx: SlashContext,
): Promise<void> {
  const lines = body
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0 && !l.startsWith("#"));
  for (const line of lines) {
    const input = line.startsWith("/") ? line : `/${line}`;
    const cmd = findCommand(input);
    if (!cmd) {
      ctx.notify(
        "Custom slash step skipped",
        `unknown command: ${input.split(/\s/, 1)[0]}`,
        "warning",
      );
      continue;
    }
    const parsed = parseInput(input);
    const args = parsed?.args ?? "";
    try {
      await cmd.run(args, ctx);
    } catch (err) {
      ctx.notify("Custom slash step failed", humanizeError(err), "error");
    }
  }
}

/**
 * Wrap a stored `CustomSlash` into a `SlashCommand` that the index can
 * route through `findCommand`. Tagged with a symbol so a later refresh
 * can find + replace its previous registration.
 */
function toSlashCommand(slash: CustomSlash): TaggedCommand {
  const cmd: TaggedCommand = {
    name: slash.name,
    description:
      slash.description || `custom: ${slash.body.split(/\r?\n/).filter(Boolean).length} step(s)`,
    run: async (_args, ctx) => {
      await runCustomBody(slash.body, ctx);
    },
  };
  cmd[CUSTOM_TAG] = true;
  return cmd;
}

/**
 * Replace every custom-tagged entry in the shared `COMMANDS` array with the
 * supplied list. Idempotent — calling this with the same list twice yields
 * the same registry. Mutates the exported array in place so the existing
 * `INDEX` builder (run once at module load) is bypassed: callers must use
 * `findCommand` only AFTER this has resolved. For v1, the kick-off happens
 * fire-and-forget at slash-commands.ts module init, so the first lookup
 * after boot may miss a freshly-defined slash; that's acceptable.
 */
export function pushCustomSlashes(slashes: CustomSlash[]): void {
  // Drop previous custom entries first so this function doubles as a
  // "reload after save" hook.
  for (let i = COMMANDS.length - 1; i >= 0; i--) {
    if (isCustom(COMMANDS[i])) COMMANDS.splice(i, 1);
  }
  // Skip names that collide with built-ins so a user-defined `/test` can't
  // shadow the real one — toast-side warning is on the panel that owns the
  // editor; here we silently drop to keep the runtime predictable.
  const builtinNames = new Set<string>();
  for (const c of COMMANDS) {
    builtinNames.add(c.name.toLowerCase());
    for (const a of c.aliases ?? []) builtinNames.add(a.toLowerCase());
  }
  for (const slash of slashes) {
    const key = slash.name.toLowerCase();
    if (builtinNames.has(key)) continue;
    COMMANDS.push(toSlashCommand(slash));
  }
  // Re-seed the lookup index so `findCommand` resolves the freshly-added
  // custom entries. Skipping this would leave the first lookup blind even
  // after a save (the INDEX builder runs once at module load).
  rebuildSlashIndex();
}
