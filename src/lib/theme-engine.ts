/**
 * Shared theme engine — the single source of truth for turning a palette into
 * the COMPLETE set of CSS custom properties on `:root`.
 *
 * Both theme systems route through this:
 *   - `themes.ts`        — curated, full palettes (Coral Dark, Slate, Paper, …)
 *   - `themes-custom.ts` — user JSON themes + built-in presets (a subset of
 *                          tokens authored by hand)
 *
 * WHY THIS EXISTS: the custom/JSON applier used to write only a handful of
 * tokens (accent, bg, text, …) and leave the rest at the dark `global.css`
 * defaults. That left every preset and user theme with holes — a dark
 * `--accent-fg` baked for the default coral (illegible on a light accent),
 * dark drop-shadows over a paper background, no derived `--border-strong` /
 * `--bg-hover`, and `data-theme-mode` never flipped to `light` so the
 * light-mode CSS overrides never fired. Those holes are exactly what reads as
 * "not professional." This module derives the full token set from whatever a
 * theme provides, so a three-color custom theme renders as coherently as a
 * curated one, while partial overrides (set only an accent) still work.
 *
 * Pure color math + DOM writes, no React, no Tauri — safe to import anywhere.
 */

/** Loose palette input. Required fields make a complete theme; the optional
 *  ones are derived when absent. Empty strings are treated as "absent" so a
 *  partial custom theme keeps falling back to the global defaults. */
export interface CoreTokens {
  bg?: string;
  bgElevated?: string;
  bgSunken?: string;
  bgHover?: string;
  border?: string;
  borderStrong?: string;
  text?: string;
  textDim?: string;
  textMuted?: string;
  accent?: string;
  accentStrong?: string;
  accentDim?: string;
  /** Foreground placed ON the accent (primary buttons). Derived if absent. */
  accentFg?: string;
  /** "r, g, b" triplet for soft/glow tints. Derived from `accent` if absent. */
  accentRgb?: string;
  success?: string;
  warning?: string;
  danger?: string;
  info?: string;
  fontSans?: string;
  fontMono?: string;
  /** Forces the elevation/shadow + `data-theme-mode` set. Derived from the
   *  background luminance when absent. */
  mode?: "dark" | "light";
}

// Layered elevation (see docs/DESIGN-SPEC.md §4). Each tier is a soft two-layer
// shadow — a tight contact shadow plus an ambient one — rather than a single
// heavy blur. Dark themes keep shadows subtle (surface + hairline border carry
// most of the depth); light themes lean on the shadow for elevation.
export const DARK_SHADOWS: Array<[string, string]> = [
  ["--shadow-sm", "0 1px 2px rgba(0, 0, 0, 0.30), 0 1px 1px rgba(0, 0, 0, 0.18)"],
  ["--shadow-md", "0 2px 4px rgba(0, 0, 0, 0.28), 0 6px 16px rgba(0, 0, 0, 0.36)"],
  ["--shadow-lg", "0 4px 8px rgba(0, 0, 0, 0.30), 0 16px 40px rgba(0, 0, 0, 0.46)"],
];
export const LIGHT_SHADOWS: Array<[string, string]> = [
  ["--shadow-sm", "0 1px 2px rgba(16, 24, 40, 0.06), 0 1px 1px rgba(16, 24, 40, 0.04)"],
  ["--shadow-md", "0 2px 4px rgba(16, 24, 40, 0.06), 0 8px 24px rgba(16, 24, 40, 0.08)"],
  ["--shadow-lg", "0 4px 8px rgba(16, 24, 40, 0.06), 0 24px 48px rgba(16, 24, 40, 0.12)"],
];

// ── Color math ──────────────────────────────────────────────────────────────

/** Parse #rgb / #rrggbb (any case, optional leading #) to [r,g,b] 0–255. */
export function parseHex(hex: string): [number, number, number] | null {
  if (typeof hex !== "string") return null;
  let h = hex.trim().replace(/^#/, "");
  if (h.length === 3) h = h.split("").map((c) => c + c).join("");
  if (h.length !== 6 || /[^0-9a-fA-F]/.test(h)) return null;
  return [
    parseInt(h.slice(0, 2), 16),
    parseInt(h.slice(2, 4), 16),
    parseInt(h.slice(4, 6), 16),
  ];
}

function toHex(rgb: [number, number, number]): string {
  return (
    "#" +
    rgb
      .map((c) => Math.max(0, Math.min(255, Math.round(c))).toString(16).padStart(2, "0"))
      .join("")
  );
}

/** WCAG relative luminance, 0 (black) … 1 (white). */
export function relativeLuminance(hex: string): number {
  const rgb = parseHex(hex);
  if (!rgb) return 0;
  const [r, g, b] = rgb.map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  const [hi, lo] = la >= lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/** True when a background is light enough to warrant light-mode chrome. */
export function isLight(hex: string): boolean {
  return relativeLuminance(hex) > 0.45;
}

/** "r, g, b" triplet for use inside rgba(). */
function rgbTriplet(hex: string): string | null {
  const rgb = parseHex(hex);
  return rgb ? rgb.join(", ") : null;
}

/** Linear blend of two hex colors (t=0 → a, t=1 → b). Good enough for
 *  deriving hairline borders / hover surfaces from a base + text color. */
function mix(a: string, b: string, t: number): string | null {
  const ra = parseHex(a);
  const rb = parseHex(b);
  if (!ra || !rb) return null;
  return toHex([
    ra[0] + (rb[0] - ra[0]) * t,
    ra[1] + (rb[1] - ra[1]) * t,
    ra[2] + (rb[2] - ra[2]) * t,
  ]);
}

/** Pick a legible foreground for text placed on the accent — near-black on a
 *  light/bright accent, white on a deep/saturated one — by max contrast. */
export function deriveAccentFg(accent: string): string {
  const dark = "#10100c";
  const light = "#ffffff";
  return contrast(accent, dark) >= contrast(accent, light) ? dark : light;
}

// ── The writer ───────────────────────────────────────────────────────────────

/**
 * Window event fired after every token write. Surfaces that hold imperative,
 * non-CSS color state (CodeMirror's `dark` flag + highlight style, xterm.js
 * theme options) listen for this and re-derive their colors from the freshly
 * written custom properties. Pure-CSS consumers don't need it — `var(--token)`
 * references update live.
 */
export const THEME_CHANGED_EVENT = "cortex:theme-changed";

function present(v: string | undefined): v is string {
  return typeof v === "string" && v.trim().length > 0;
}

/**
 * Write the full token set onto a root element. Every token is only written
 * when its source value is present (or derivable), so:
 *   - a complete palette produces a hole-free theme, and
 *   - a partial custom theme (e.g. accent only) overrides just that token and
 *     leaves the rest at the global defaults.
 *
 * Returns the resolved mode ("dark"/"light") when it could be determined, so
 * callers can mirror it onto `data-theme-mode` / `colorScheme` if they want a
 * different element than the one written to.
 *
 * CONTRACT: callers pass `document.documentElement` (= `:root`) so the active
 * palette lands on `:root` ITSELF, not just `body`. An inline custom property on
 * the root element surfaces in `getComputedStyle(:root)`, so this guarantees
 * `getComputedStyle(document.documentElement).getPropertyValue("--bg")` reads the
 * ACTIVE theme rather than the `global.css` `:root {}` default block — which both
 * keeps the visible surface (body + descendants inherit via `var()`) correct AND
 * lets the e2e probe assert active==painted on `:root` (see e2e-probe.ts
 * `rootReflectsActive`). Writing to `body` instead would paint correctly but
 * leave `:root` reading the defaults — don't.
 */
export function applyCoreTokens(
  root: HTMLElement,
  t: CoreTokens,
): "dark" | "light" | undefined {
  const set = (k: string, v: string | undefined) => {
    if (present(v)) root.style.setProperty(k, v);
  };

  // Surfaces.
  set("--bg", t.bg);
  set("--bg-elevated", t.bgElevated);
  set("--bg-elev", t.bgElevated);
  set("--bg-sunken", t.bgSunken);

  // Hover surface + hairline borders derive from bg×text when not given —
  // so they track light vs dark automatically instead of staying dark.
  const bgHover = present(t.bgHover)
    ? t.bgHover
    : present(t.bg) && present(t.text)
      ? mix(t.bg, t.text, 0.06) ?? undefined
      : undefined;
  const border = present(t.border)
    ? t.border
    : present(t.bg) && present(t.text)
      ? mix(t.bg, t.text, 0.13) ?? undefined
      : undefined;
  const borderStrong = present(t.borderStrong)
    ? t.borderStrong
    : present(t.bg) && present(t.text)
      ? mix(t.bg, t.text, 0.26) ?? undefined
      : undefined;
  set("--bg-hover", bgHover);
  set("--border", border);
  set("--border-strong", borderStrong);

  // Text — mirror onto the --fg aliases used by chrome/labels.
  set("--text", t.text);
  set("--fg", t.text);
  set("--text-dim", t.textDim);
  set("--fg-dim", t.textDim);
  set("--text-muted", t.textMuted);
  set("--fg-muted", t.textMuted);

  // Accent + derived foreground/tints.
  set("--accent", t.accent);
  set("--accent-strong", t.accentStrong);
  set("--accent-dim", t.accentDim);
  const accentFg = present(t.accentFg)
    ? t.accentFg
    : present(t.accent)
      ? deriveAccentFg(t.accent)
      : undefined;
  set("--accent-fg", accentFg);
  if (present(t.accent)) {
    const rgb = present(t.accentRgb) ? t.accentRgb : rgbTriplet(t.accent);
    if (rgb) {
      set("--accent-soft", `rgba(${rgb}, 0.10)`);
      set("--accent-softer", `rgba(${rgb}, 0.05)`);
      set("--accent-glow", `rgba(${rgb}, 0.25)`);
    }
  }

  // Semantic colors + the foregrounds painted ON those fills (status badges,
  // trace bars, danger-button hover). Derived per-theme exactly like
  // --accent-fg — a max-contrast near-black/white pick — so text on a
  // semantic fill stays legible when a theme redefines --warning/--success/
  // --danger/--info toward darker values, instead of relying on hardcoded
  // dark literals that only worked on the stock pastel semantics.
  set("--success", t.success);
  set("--warning", t.warning);
  set("--danger", t.danger);
  set("--info", t.info);
  set("--success-fg", present(t.success) ? deriveAccentFg(t.success) : undefined);
  set("--warning-fg", present(t.warning) ? deriveAccentFg(t.warning) : undefined);
  set("--danger-fg", present(t.danger) ? deriveAccentFg(t.danger) : undefined);
  set("--info-fg", present(t.info) ? deriveAccentFg(t.info) : undefined);

  // Elevation tiers — nudge each step off the base so layered surfaces stay
  // distinguishable in every theme.
  set("--bg-elev-1", t.bgElevated);
  set("--bg-elev-2", bgHover);
  set("--bg-elev-3", borderStrong);

  // Fonts (custom themes may override).
  set("--font-sans", t.fontSans);
  set("--font-mono", t.fontMono);

  // Mode-dependent: shadows + the data attribute that gates light-mode CSS.
  const mode: "dark" | "light" | undefined =
    t.mode ?? (present(t.bg) ? (isLight(t.bg) ? "light" : "dark") : undefined);
  if (mode) {
    const shadows = mode === "light" ? LIGHT_SHADOWS : DARK_SHADOWS;
    for (const [k, v] of shadows) root.style.setProperty(k, v);
    root.dataset.themeMode = mode;
    root.style.colorScheme = mode;
  }

  // Tell live, imperatively-colored surfaces (editor, terminal) to re-read
  // the tokens. Dispatched after every write so a theme applied from either
  // system (curated palettes or custom JSON themes) propagates everywhere.
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(THEME_CHANGED_EVENT));
  }
  return mode;
}

/**
 * Boot token cache — the single source of truth for the FIRST PAINT.
 *
 * There are two live theme systems (`applyTheme` over the curated `PALETTES`
 * and `applyCustomTheme` over user/preset themes) and historically each
 * persisted to a different store: `localStorage["cortex.theme"]` (a palette id)
 * vs the backend `~/.cortex/themes.json` (a theme name). On boot the legacy
 * system synchronously painted its localStorage palette, then `useThemeBoot`
 * asynchronously repainted the backend theme — and when the two stores had
 * drifted (they do: onboarding writes the legacy store, the picker writes the
 * backend) the user saw a flash of the WRONG theme on every launch (e.g.
 * cyber-red → solarized-light). That flash is exactly the kind of thing that
 * reads as "not professional."
 *
 * Fix: both appliers funnel the EXACT tokens they just wrote into this one
 * cache, and boot replays the cache synchronously before first paint — so the
 * first frame already shows the last theme the user actually applied, whichever
 * UI set it. `useThemeBoot` still reconciles against the authoritative backend
 * afterwards (and refreshes the cache), so a theme changed out-of-band heals on
 * the next frame instead of flashing every time. The key lives under the
 * `cortex.*` namespace so `pref-sync` mirrors it to disk and it survives the
 * post-update localStorage wipe.
 */
const TOKEN_CACHE_KEY = "cortex.theme.tokens.v1";

/** Persist the last-applied core tokens for a flash-free next boot. */
export function cacheCoreTokens(t: CoreTokens): void {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(TOKEN_CACHE_KEY, JSON.stringify(t));
    }
  } catch {
    /* storage unavailable — boot just falls back to the global.css default */
  }
}

/**
 * Apply the cached core tokens synchronously. Returns true when a usable cache
 * existed and was applied. Call before first paint; a miss (fresh install, or
 * the first boot after this landed) leaves the `global.css` default in place,
 * which `useThemeBoot` then corrects from the backend.
 */
export function applyCachedCoreTokens(root: HTMLElement): boolean {
  try {
    if (typeof localStorage === "undefined") return false;
    const raw = localStorage.getItem(TOKEN_CACHE_KEY);
    if (!raw) return false;
    const t = JSON.parse(raw) as CoreTokens;
    // A real theme always carries a background; reject anything malformed so a
    // tampered/empty cache can't blank the surfaces.
    if (!t || typeof t !== "object" || !present(t.bg)) return false;
    applyCoreTokens(root, t);
    return true;
  } catch {
    return false;
  }
}
