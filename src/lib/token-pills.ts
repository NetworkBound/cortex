/**
 * Pill-token rendering for the Omnibar.
 *
 * The Omnibar uses a contenteditable div so it can show colored chips for
 * `@file:…`, `#snippet:…`, `/web …` envelopes while still behaving like an
 * input. This module is purely string⇄HTML — no DOM mutation or React.
 *
 * Visual scheme is lifted from Terax-AI: each kind gets a distinct hue plus
 * a single leading glyph so the eye can scan them. The chip itself carries
 * `data-token` with the canonical text so `extractPlainText()` can reverse it
 * losslessly for sending to the backend.
 *
 * Visual pattern: Terax-AI pill-token rendering (2026-05).
 */

export type TokenKind =
  | "plain"
  | "file"
  | "diff"
  | "memory"
  | "docs"
  | "folder"
  | "symbol"
  | "thread"
  | "git"
  | "at-other"
  | "snippet"
  | "slash";

export interface Token {
  kind: TokenKind;
  /** Raw matched text, e.g. `@file:src/foo.tsx` or `#snippet:save-test`. */
  text: string;
  /** Just the value portion (after the prefix), e.g. `src/foo.tsx`. */
  value: string;
}

/** Map `@xxx` envelope keyword to a token kind + chip glyph. */
const AT_KIND: Record<string, { kind: TokenKind; icon: string }> = {
  file: { kind: "file", icon: "📄" },
  files: { kind: "file", icon: "📄" },
  diff: { kind: "diff", icon: "±" },
  memory: { kind: "memory", icon: "🧠" },
  mem: { kind: "memory", icon: "🧠" },
  docs: { kind: "docs", icon: "📚" },
  doc: { kind: "docs", icon: "📚" },
  folder: { kind: "folder", icon: "📁" },
  folders: { kind: "folder", icon: "📁" },
  dir: { kind: "folder", icon: "📁" },
  symbol: { kind: "symbol", icon: "ƒ" },
  symbols: { kind: "symbol", icon: "ƒ" },
  sym: { kind: "symbol", icon: "ƒ" },
  thread: { kind: "thread", icon: "❖" },
  threads: { kind: "thread", icon: "❖" },
  git: { kind: "git", icon: "⎇" },
};

// Token regexes — note the ORDER matters because we scan left-to-right and
// the longest specific match wins at each position. We keep them anchored
// with a global flag so we can iterate with `matchAll`.
const TOKEN_RE = /(@(\w+):([^\s]+))|(#([\w.-]+)(?::([^\s]*))?)|(\/\w+)/g;

/** Parse a raw text string into an ordered list of tokens. */
export function parseTokens(text: string): Token[] {
  const tokens: Token[] = [];
  let cursor = 0;
  // matchAll handles all three alternations in a single left-to-right pass.
  for (const m of text.matchAll(TOKEN_RE)) {
    const start = m.index ?? 0;
    if (start > cursor) {
      tokens.push({ kind: "plain", text: text.slice(cursor, start), value: "" });
    }
    const raw = m[0];
    if (m[1]) {
      // @keyword:value
      const keyword = (m[2] ?? "").toLowerCase();
      const value = m[3] ?? "";
      const hit = AT_KIND[keyword];
      tokens.push({
        kind: hit ? hit.kind : "at-other",
        text: raw,
        value,
      });
    } else if (m[4]) {
      // #keyword[:value]
      tokens.push({
        kind: "snippet",
        text: raw,
        value: m[6] ?? m[5] ?? "",
      });
    } else if (m[7]) {
      // /command
      tokens.push({
        kind: "slash",
        text: raw,
        value: raw.slice(1),
      });
    }
    cursor = start + raw.length;
  }
  if (cursor < text.length) {
    tokens.push({ kind: "plain", text: text.slice(cursor), value: "" });
  }
  return tokens;
}

/** Minimal HTML escape — chips never contain user-controlled HTML. */
function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/**
 * Return the glyph displayed inside a chip for a given token. Most kinds map
 * via AT_KIND; snippet/slash have their own glyphs.
 */
function chipIcon(token: Token): string {
  switch (token.kind) {
    case "snippet":
      return "#";
    case "slash":
      return "/";
    case "at-other":
      return "@";
    default: {
      // Find the icon by reverse-looking the @-kind table.
      for (const [, v] of Object.entries(AT_KIND)) {
        if (v.kind === token.kind) return v.icon;
      }
      return "@";
    }
  }
}

/**
 * Return the short label shown to the right of the chip glyph. For files we
 * use just the basename so long paths don't blow up the input height.
 */
function chipLabel(token: Token): string {
  if (token.kind === "slash") return token.value;
  if (token.kind === "snippet") return token.value || token.text.slice(1);
  // For at-envelopes, prefer basename of path-like values.
  const v = token.value;
  if (token.kind === "file" || token.kind === "folder" || token.kind === "docs" || token.kind === "memory") {
    const slash = v.lastIndexOf("/");
    return slash >= 0 ? v.slice(slash + 1) : v;
  }
  return v;
}

/**
 * Render tokens to HTML suitable for assignment to a contenteditable div.
 *
 * Plain runs are wrapped in `<span>` so the browser doesn't collapse runs of
 * whitespace or generate `<br>` artifacts when the user hits space. Chips
 * carry `data-token` with the canonical text so the reverse operation is
 * lossless. `contenteditable=false` on the chip itself prevents the caret
 * from entering the middle of a chip (browsers handle this consistently).
 */
export function renderTokensToHTML(tokens: Token[]): string {
  const parts: string[] = [];
  for (const t of tokens) {
    if (t.kind === "plain") {
      // Preserve spaces by converting them to NBSP-safe entities only when
      // they would otherwise collapse at chip boundaries.
      parts.push(`<span class="pill-plain">${esc(t.text)}</span>`);
    } else {
      const klass = `pill pill-${t.kind}`;
      const icon = chipIcon(t);
      const label = chipLabel(t);
      parts.push(
        `<span class="${klass}" contenteditable="false" data-token="${esc(t.text)}" title="${esc(t.text)}">` +
          `<span class="pill-icon">${esc(icon)}</span>` +
          `<span class="pill-label">${esc(label)}</span>` +
          `</span>`,
      );
    }
  }
  // Trailing empty span ensures the caret can sit after the last chip.
  parts.push(`<span class="pill-plain pill-tail"></span>`);
  return parts.join("");
}

/** Decode the minimal set of HTML entities produced by `esc()`. */
function unesc(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&amp;/g, "&");
}

/**
 * DOM-free reverse of `renderTokensToHTML` for SSR/tests. Walks the markup
 * tag-by-tag. A chip is an outer `<span … data-token="…">` wrapping nested
 * icon/label spans; on entering one we emit the decoded `data-token` and skip
 * its entire subtree by counting span open/close tags until depth returns to
 * zero. Non-chip text nodes are emitted (decoded) as-is.
 */
function extractPlainTextNoDom(html: string): string {
  const out: string[] = [];
  // Matches a span open tag, any other tag, or a run of non-tag text.
  const re = /<span\b([^>]*)>|<\/span\s*>|<[^>]+>|([^<]+)/gi;
  let m: RegExpExecArray | null;
  let skipDepth = 0; // >0 while inside a chip subtree we're discarding.
  while ((m = re.exec(html)) !== null) {
    const full = m[0];
    if (skipDepth > 0) {
      // Inside a chip: only track span nesting so we know when it closes.
      if (m[1] !== undefined) skipDepth++;
      else if (/^<\/span/i.test(full)) skipDepth--;
      continue;
    }
    if (m[1] !== undefined) {
      // Opening <span ...>. If it's a chip, emit its data-token and skip body.
      const attrs = m[1];
      const tokMatch = /\bdata-token="([^"]*)"/i.exec(attrs);
      if (tokMatch) {
        out.push(unesc(tokMatch[1]));
        skipDepth = 1;
      }
      // Non-chip spans contribute nothing themselves; their text follows.
    } else if (m[2] !== undefined) {
      // Raw text run.
      out.push(unesc(m[2]));
    }
    // Other tags (including non-chip </span>) are dropped.
  }
  return out.join("");
}

/**
 * Reverse the rendering: turn the contenteditable HTML back into the
 * canonical text. We rely on `data-token` for chips (lossless) and on
 * `textContent` for everything else. This is what we send to the backend.
 *
 * Implementation reads the HTML through a detached DOM node so we don't
 * have to write a parser; the Omnibar window already has a DOM.
 */
export function extractPlainText(html: string): string {
  if (typeof document === "undefined") {
    // Fallback for SSR/tests: best-effort strip of tags. Chips are nested
    // spans (icon + label) so a non-greedy `</span>` match would stop at the
    // first inner closing tag and leak the chip's inner text. Instead we scan
    // tag-by-tag, and when we enter a chip (a span carrying `data-token`) we
    // emit the decoded token and skip everything up to the matching closing
    // `</span>`, tracking span depth so nested spans don't end the chip early.
    return extractPlainTextNoDom(html);
  }
  const container = document.createElement("div");
  container.innerHTML = html;
  const out: string[] = [];
  const walk = (node: Node) => {
    if (node.nodeType === Node.TEXT_NODE) {
      out.push(node.textContent ?? "");
      return;
    }
    if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as HTMLElement;
      const tok = el.getAttribute("data-token");
      if (tok) {
        out.push(tok);
        return;
      }
      for (const child of Array.from(el.childNodes)) walk(child);
    }
  };
  for (const child of Array.from(container.childNodes)) walk(child);
  return out.join("");
}

/**
 * Render a raw text string straight to chip HTML — small convenience for
 * callers that already hold the plain text (e.g. setting initial value).
 */
export function renderTextToHTML(text: string): string {
  return renderTokensToHTML(parseTokens(text));
}
