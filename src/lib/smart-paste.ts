// Smart paste — heuristic helpers for the chat composer.
//
// When the user pastes a chunk of text larger than a few sentences, we surface
// a tiny floating action menu (rendered by ChatPane) offering quick
// transformations: fence-wrap the buffer with a language hint, save it as a
// snippet, or normalize whitespace. Everything here is pure / synchronous so
// the menu stays snappy and unit-testable.

/** Minimum character count that opts a paste into the smart-paste flow. */
export const SMART_PASTE_MIN_CHARS = 200;

/** Minimum line count (alternative trigger) for shorter but multiline pastes. */
export const SMART_PASTE_MIN_LINES = 5;

/**
 * Returns true when a pasted blob is large enough to be worth offering the
 * smart-paste menu. Two triggers: long-enough text (>200 chars) OR a multiline
 * snippet (>5 lines), since a 6-line shell transcript is worth fencing even if
 * it's well under 200 chars.
 */
export function shouldOfferSmartPaste(text: string): boolean {
  if (text.length > SMART_PASTE_MIN_CHARS) return true;
  // Count newlines once — cheaper than splitting.
  let nl = 0;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) nl++;
  }
  return nl + 1 > SMART_PASTE_MIN_LINES;
}

// Language detection patterns. We sample the first non-blank lines and look
// for tell-tale prefixes. Order matters — more specific patterns first (e.g.
// `from … import` before bare `import`) so Python beats JS on Python source.
interface LangPattern {
  lang: string;
  // Tested against each of the first ~10 non-blank lines; first hit wins.
  test: RegExp;
}

const LANG_PATTERNS: LangPattern[] = [
  // Python
  { lang: "python", test: /^\s*from\s+[\w.]+\s+import\b/ },
  { lang: "python", test: /^\s*def\s+\w+\s*\(/ },
  { lang: "python", test: /^\s*class\s+\w+(?:\s*\([^)]*\))?\s*:/ },
  { lang: "python", test: /^\s*if\s+__name__\s*==\s*['"]__main__['"]/ },
  // Rust
  { lang: "rust", test: /^\s*use\s+\w+(?:::\w+)*\s*(?:::\{|;)/ },
  { lang: "rust", test: /^\s*fn\s+\w+\s*[(<]/ },
  { lang: "rust", test: /^\s*(?:pub\s+)?(?:struct|enum|trait|impl)\s+\w+/ },
  { lang: "rust", test: /^\s*let\s+(?:mut\s+)?\w+\s*(?::\s*\w+)?\s*=/ },
  // TypeScript (checked before JS so `: Type` and `interface` win)
  { lang: "typescript", test: /^\s*(?:export\s+)?interface\s+\w+/ },
  { lang: "typescript", test: /^\s*(?:export\s+)?type\s+\w+\s*=/ },
  { lang: "typescript", test: /^\s*import\s+type\s+/ },
  { lang: "typescript", test: /:\s*(?:string|number|boolean|void|unknown|any)\b/ },
  // JavaScript
  { lang: "javascript", test: /^\s*import\s+(?:\{[^}]*\}|\w+|\*\s+as\s+\w+)\s+from\s+['"]/ },
  { lang: "javascript", test: /^\s*(?:export\s+)?(?:async\s+)?function\s+\w+\s*\(/ },
  { lang: "javascript", test: /^\s*const\s+\w+\s*=\s*(?:async\s*)?\(/ },
  { lang: "javascript", test: /^\s*require\s*\(\s*['"]/ },
  // Shell
  { lang: "bash", test: /^#!\s*\/(?:usr\/)?bin\/(?:env\s+)?(?:bash|sh|zsh)/ },
  { lang: "bash", test: /^\s*\$\s+\w/ },
  // SQL
  { lang: "sql", test: /^\s*(?:SELECT|INSERT|UPDATE|DELETE|CREATE|ALTER|DROP)\s+/i },
  // JSON (object/array start, no trailing semicolons on the first lines)
  { lang: "json", test: /^\s*[{[]\s*$/ },
  // YAML
  { lang: "yaml", test: /^---\s*$/ },
  // HTML / XML
  { lang: "html", test: /^\s*<!DOCTYPE\s+html/i },
  { lang: "html", test: /^\s*<(?:html|head|body|div|span|p|a)\b/i },
];

/**
 * Heuristically guess a fenced-code language from the pasted text. Returns ""
 * when nothing matches confidently — callers should treat the empty string as
 * "no hint" rather than guessing further.
 *
 * Sampling: only the first 10 non-blank lines are tested. This is enough to
 * fingerprint most files without paying a full scan on multi-megabyte pastes.
 */
export function detectLanguageFromContent(text: string): string {
  if (!text) return "";
  const lines: string[] = [];
  // Walk lines until we have 10 non-blank ones (or run out).
  let start = 0;
  for (let i = 0; i <= text.length && lines.length < 10; i++) {
    if (i === text.length || text.charCodeAt(i) === 10) {
      const line = text.slice(start, i);
      if (line.trim().length > 0) lines.push(line);
      start = i + 1;
    }
  }
  for (const { lang, test } of LANG_PATTERNS) {
    for (const line of lines) {
      if (test.test(line)) return lang;
    }
  }
  return "";
}

/**
 * Wrap `text` in a fenced code block, optionally tagged with a language hint.
 * Picks a fence length one tick longer than the longest run of backticks
 * inside the body so we never accidentally close the fence early.
 */
export function wrapInFence(text: string, language: string): string {
  // Longest run of consecutive backticks in the body — fence must be longer.
  let longest = 0;
  let run = 0;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 96 /* ` */) {
      run += 1;
      if (run > longest) longest = run;
    } else {
      run = 0;
    }
  }
  const fenceLen = Math.max(3, longest + 1);
  const fence = "`".repeat(fenceLen);
  const trimmedEnd = text.replace(/\s+$/u, "");
  const hint = language.trim();
  return `${fence}${hint}\n${trimmedEnd}\n${fence}`;
}

/**
 * Collapse extra whitespace in a paste:
 *  - normalize CRLF → LF
 *  - strip trailing spaces/tabs from each line
 *  - collapse 3+ consecutive blank lines down to a single blank line
 *  - trim leading/trailing blank lines from the whole block
 *
 * Leaves single-blank-line gaps intact so paragraph structure survives.
 */
export function trimWhitespace(text: string): string {
  const normalized = text.replace(/\r\n?/g, "\n");
  const trimmedLines = normalized
    .split("\n")
    .map((line) => line.replace(/[\t ]+$/u, ""));
  // Collapse runs of blank lines (length 0 after the trim above) to at most 1.
  const out: string[] = [];
  let blankRun = 0;
  for (const line of trimmedLines) {
    if (line.length === 0) {
      blankRun += 1;
      if (blankRun <= 1) out.push(line);
    } else {
      blankRun = 0;
      out.push(line);
    }
  }
  // Drop leading / trailing blanks from the whole block.
  while (out.length > 0 && out[0].length === 0) out.shift();
  while (out.length > 0 && out[out.length - 1].length === 0) out.pop();
  return out.join("\n");
}
