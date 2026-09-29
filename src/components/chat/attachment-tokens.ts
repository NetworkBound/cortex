/**
 * Shared @-token / implicit-mention matchers for the chat composer and
 * message bubbles. These mirror the shapes the backend's `expand_at_tokens`
 * recognises; keep them in lock-step with `src-tauri` when adding a token.
 *
 * Three call sites need slightly different strictness, so three regexes:
 *  - `AT_TOKEN_RE`      — a token WITH its value, for the pre-send preview.
 *  - `AT_TOKEN_LOOSE_RE` — prefix-only (`@web:` counts), for "did the user
 *                          already express context intent" in the brain gate.
 *  - `AT_CHIP_RE`       — the persisted-message chip parser (no folder/dir).
 *  - `IMPLICIT_MENTION_RE` — Aider-style bare relative paths with a known
 *                          code/doc extension (max 3 are auto-attached).
 */

export const AT_TOKEN_RE =
  /@(?:brain|diff|status|recent|repomap|cwd|env|ls|log)(?::[^\s,;)]*)?\b|@(?:memory|file|frag|web|grep|folder|dir|blame):[^\s,;)]+|@[/\\][^\s]+/g;

export const AT_TOKEN_LOOSE_RE =
  /@(?:brain|diff|status|recent|repomap|cwd|env|ls|log)(?::[^\s,;)]*)?\b|@(?:memory|file|frag|web|grep|folder|dir|blame):|@[/\\]/g;

export const AT_CHIP_RE =
  /@(?:brain|diff|status|recent|repomap|cwd|env|ls|log)(?::[^\s,;)]*)?\b|@(?:memory|file|frag|web|grep|blame):[^\s,;)]+|@[/\\][^\s,;)]+/g;

export const IMPLICIT_MENTION_RE =
  /\b[\w.-]+(?:[/\\][\w.-]+)+\.(?:rs|ts|tsx|js|jsx|py|go|java|kt|c|cc|cpp|h|hpp|rb|php|swift|scala|md|toml|yaml|yml|json|css|scss|html|sh|sql|proto|gradle|zig|dart|elm|json5|lua|nix|tf|mjs|cjs|astro|vue|svelte|jl|ex|exs|clj|hs|ml)(?::\d+(?::\d+)?)?\b/g;

/** Backend cap on implicit path mentions auto-attached per message. */
export const IMPLICIT_MENTION_CAP = 3;

/**
 * Tokens + implicit mentions the backend will resolve for `draft`, for the
 * "N attachments queued" preview under the composer. `match` with a /g regex
 * is stateless, so the shared constants are safe to reuse here.
 */
export function previewAttachments(draft: string): string[] {
  const tokens = draft.match(AT_TOKEN_RE) ?? [];
  const mentions = (draft.match(IMPLICIT_MENTION_RE) ?? []).slice(
    0,
    IMPLICIT_MENTION_CAP,
  );
  return [...tokens, ...mentions];
}

/** Count of explicit + implicit context references in a draft (brain gate). */
export function contextIntentCount(draft: string): number {
  const tokens = (draft.match(AT_TOKEN_LOOSE_RE) ?? []).length;
  const mentions = (draft.match(IMPLICIT_MENTION_RE) ?? []).length;
  return tokens + mentions;
}

export interface AttachmentChip {
  label: string;
  /** True for an implicit path mention (rendered amber, with a tooltip). */
  mention: boolean;
}

/**
 * Compact chips for a persisted user message: explicit tokens (capped at 8,
 * paths shortened to their basename) followed by up to 3 implicit mentions.
 */
export function attachmentChips(content: string): AttachmentChip[] {
  const out: AttachmentChip[] = [];
  const re = new RegExp(AT_CHIP_RE.source, "g");
  let mm: RegExpExecArray | null;
  while ((mm = re.exec(content)) !== null) {
    const tok = mm[0];
    if (tok.includes(":")) {
      const [head, ...rest] = tok.split(":");
      const tail = rest.join(":");
      const short = tail.split(/[/\\]/).pop() ?? tail;
      out.push({ label: `${head}:${short}`, mention: false });
    } else if (tok.startsWith("@/") || tok.startsWith("@\\")) {
      const short = tok.split(/[/\\]/).pop() ?? tok;
      out.push({ label: `@${short}`, mention: false });
    } else {
      out.push({ label: tok, mention: false });
    }
    if (out.length >= 8) break;
  }
  const mentionRe = new RegExp(IMPLICIT_MENTION_RE.source, "g");
  let mn: RegExpExecArray | null;
  let mentions = 0;
  while ((mn = mentionRe.exec(content)) !== null) {
    const tail = mn[0].split(/[/\\]/).pop() ?? mn[0];
    out.push({ label: tail, mention: true });
    mentions += 1;
    if (mentions >= IMPLICIT_MENTION_CAP) break;
    if (out.length >= 11) break;
  }
  return out;
}
