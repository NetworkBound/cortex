/**
 * Centralized keyboard shortcut binding registry.
 *
 * Bindings are identified by stable string ids (`send`, `palette`, …) so call
 * sites don't hard-code key combos; {@link DEFAULT_KEYMAP} is the single
 * source of truth for the combos and {@link matchCombo} matches key events
 * against them.
 *
 * NOTE: this module intentionally has no React / store coupling — it is a
 * pure data + parsing module so it can be unit-tested and reused.
 */

export type KeymapBinding = {
  id: string;
  combo: string;
  description: string;
};

export const DEFAULT_KEYMAP: KeymapBinding[] = [
  { id: "send", combo: "Ctrl+Enter", description: "Send the current message" },
  { id: "palette", combo: "Ctrl+K", description: "Open command palette" },
  {
    id: "quickopen",
    combo: "Ctrl+P",
    description: "Quick open file/memory/session",
  },
  { id: "shortcuts", combo: "Ctrl+/", description: "Show keyboard shortcuts" },
  { id: "cycle-theme", combo: "Ctrl+T", description: "Cycle through themes" },
  {
    id: "compact",
    combo: "Ctrl+Shift+C",
    description: "Compact older messages",
  },
  {
    id: "new-session",
    combo: "Ctrl+N",
    description: "Start a new chat session",
  },
  {
    id: "new-window",
    combo: "Ctrl+Shift+N",
    description: "Open a new Cortex window",
  },
  { id: "settings", combo: "Ctrl+,", description: "Open settings" },
  { id: "cycle-mode", combo: "Ctrl+M", description: "Toggle Plan / Act mode" },
  {
    id: "focus-approval",
    combo: "Ctrl+Shift+A",
    description: "Jump to the oldest pending approval",
  },
  { id: "resume", combo: "Ctrl+R", description: "Resume a chat session" },
  {
    id: "memory-search",
    combo: "Ctrl+Shift+F",
    description: "Focus memory search",
  },
  {
    id: "cycle-tab",
    combo: "Ctrl+Tab",
    description: "Cycle activity panels (Shift reverses)",
  },
];

/** Combo string for a binding id, or `fallback` when the id is unknown. */
export function comboFor(id: string, fallback = ""): string {
  return DEFAULT_KEYMAP.find((b) => b.id === id)?.combo ?? fallback;
}

/**
 * True when the keystroke landed in an editable field (input, textarea,
 * contentEditable). Global single-modifier shortcuts skip those so a literal
 * keystroke still reaches the composer; Ctrl+Shift chords are safe to honor
 * everywhere because no editable field consumes them for text entry.
 */
export function isEditableTarget(e: KeyboardEvent): boolean {
  const target = e.target as HTMLElement | null;
  const tag = target?.tagName?.toLowerCase();
  return (
    tag === "input" ||
    tag === "textarea" ||
    tag === "select" ||
    !!target?.isContentEditable
  );
}

/** True when `combo` requires both Ctrl (or Cmd) and Shift. */
export function isCtrlShiftCombo(combo: string): boolean {
  const c = parseCombo(combo);
  return (c.ctrl || c.meta) && c.shift;
}

const KEY_LABELS: Record<string, string> = {
  enter: "Enter",
  escape: "Esc",
  space: "Space",
  tab: "Tab",
  delete: "Del",
  backspace: "Backspace",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
};

/**
 * Split a combo into display tokens for the platform the app runs on:
 * `"Ctrl+Shift+A"` → `["Ctrl", "Shift", "A"]` on Windows/Linux and
 * `["⌘", "⇧", "A"]` on macOS (where {@link matchCombo} accepts Cmd for Ctrl).
 */
export function comboParts(combo: string): string[] {
  const c = parseCombo(combo);
  const mac = isMac();
  const out: string[] = [];
  if (c.ctrl || c.meta) out.push(mac ? "⌘" : "Ctrl");
  if (c.alt) out.push(mac ? "⌥" : "Alt");
  if (c.shift) out.push(mac ? "⇧" : "Shift");
  if (c.key) {
    out.push(
      KEY_LABELS[c.key] ?? (c.key.length === 1 ? c.key.toUpperCase() : c.key),
    );
  }
  return out;
}

/** One-line, platform-correct hint text: `"Ctrl+K"` or `"⌘K"`. */
export function formatCombo(combo: string): string {
  const parts = comboParts(combo);
  return isMac() ? parts.join("") : parts.join("+");
}

interface ParsedCombo {
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  meta: boolean;
  /** Normalized key, lowercased. `"enter"`, `","`, `"space"`, `"/"`, etc. */
  key: string;
}

function parseCombo(combo: string): ParsedCombo {
  const out: ParsedCombo = {
    ctrl: false,
    shift: false,
    alt: false,
    meta: false,
    key: "",
  };
  // Split on "+", but a literal "+" key produces an empty part (e.g. "Ctrl++"
  // splits to ["Ctrl", "", ""]). Map those empties back to the "+" key instead
  // of dropping them, so binding to "+" still works.
  const parts = combo
    .split("+")
    .map((p) => p.trim())
    .map((p) => (p.length === 0 ? "+" : p));
  for (const part of parts) {
    const lc = part.toLowerCase();
    switch (lc) {
      case "ctrl":
      case "control":
        out.ctrl = true;
        break;
      case "shift":
        out.shift = true;
        break;
      case "alt":
      case "option":
        out.alt = true;
        break;
      case "meta":
      case "cmd":
      case "command":
      case "super":
      case "win":
        out.meta = true;
        break;
      default:
        out.key = normalizeKey(lc);
    }
  }
  return out;
}

/**
 * Best-effort macOS detection. On macOS the platform convention is Cmd (meta)
 * rather than Ctrl, so we alias the two there — but only there. Falls back to
 * `false` in non-browser/test contexts where `navigator` is unavailable.
 */
export function isMac(): boolean {
  if (typeof navigator === "undefined") return false;
  const platform =
    (navigator as Navigator & { userAgentData?: { platform?: string } })
      .userAgentData?.platform ||
    navigator.platform ||
    navigator.userAgent ||
    "";
  return /mac/i.test(platform);
}

function normalizeKey(key: string): string {
  // KeyboardEvent.key values can be friendly ("Enter", "ArrowUp", " ") or
  // a literal character. Map the common spellings to a single canonical form
  // so combo strings stay human-readable.
  switch (key) {
    case " ":
    case "space":
    case "spacebar":
      return "space";
    case "esc":
    case "escape":
      return "escape";
    case "return":
      return "enter";
    case "del":
      return "delete";
    default:
      return key;
  }
}

/**
 * True if `e` matches `combo`. Modifier requirements are strict — a combo of
 * `"Ctrl+K"` will not fire when the user also holds Shift, which prevents
 * accidental collisions with `"Ctrl+Shift+K"`.
 *
 * On macOS, Cmd is accepted in place of Ctrl so the same combo string works
 * cross-platform without per-OS configuration.
 */
export function matchCombo(e: KeyboardEvent, combo: string): boolean {
  const target = parseCombo(combo);
  const eventKey = normalizeKey(e.key.toLowerCase());

  if (target.key !== eventKey) return false;

  if (isMac()) {
    // On macOS, treat Cmd as an alias for Ctrl so Mac users get the same
    // combos without per-OS configuration.
    const wantCmdLike = target.ctrl || target.meta;
    const hasCmdLike = e.ctrlKey || e.metaKey;
    if (wantCmdLike !== hasCmdLike) return false;
  } else {
    // Elsewhere, Ctrl and Meta are distinct modifiers and matched strictly.
    if (target.ctrl !== e.ctrlKey) return false;
    if (target.meta !== e.metaKey) return false;
  }

  if (target.shift !== e.shiftKey) return false;
  if (target.alt !== e.altKey) return false;

  return true;
}
