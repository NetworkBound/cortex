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
];

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
function isMac(): boolean {
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
