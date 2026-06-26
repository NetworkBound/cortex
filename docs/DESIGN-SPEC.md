# Cortex — Design Spec

The reference contract for Cortex's visual design. Goal: a calm, dense,
genuinely professional desktop AI tool — the bar set by **Linear, Raycast,
Zed, Vercel, Cursor, and Superhuman**. Every component should be able to point
at a token here rather than inventing a value.

> Status: living document. The token foundation (§2–§5) is implemented in
> `src/styles/global.css` (`:root`) and `src/lib/themes.ts`. Component adoption
> (§7) is rolling out incrementally.

---

## 1. Principles (what "professional" means here)

Distilled from the reference tools:

1. **Neutrals do the work; the accent is a guest.** One accent, reserved for
   primary actions, focus, active/selected state, and key data. Never as a
   decorative wash. Semantic colors (success/warning/danger) appear only on
   actual status — never as ambient chrome. (Linear, Vercel, Zed.)
2. **Density with air.** Desktop tooling is information-dense, but every region
   has consistent internal padding and a clear rhythm. Cramped ≠ dense;
   inconsistent ≠ rich. (Linear, Superhuman.)
3. **One spacing system, one type scale.** Every gap/padding is a step on an
   8pt-derived scale; every font-size is a named scale step. No magic numbers.
4. **Elevation is layered and subtle.** Depth comes from a lighter surface + a
   hairline border + a *soft, multi-layer* shadow — not a single heavy blur.
   Dark themes lean on surface/border contrast; light themes lean on shadow.
   (Raycast, Vercel, macOS.)
5. **Restrained, fast motion.** 90–240ms, ease-out for enter, no bounce. Motion
   confirms an action; it never performs. (Linear, Raycast.)
6. **Crisp focus, always visible.** A single consistent keyboard focus ring on
   every interactive element. Accessibility is not optional.
7. **Borders are hairlines.** 1px, low-contrast. Two weights only: a default
   hairline and a "strong" divider for structural separation.

### Don'ts (the things that read as amateur)
- Multiple saturated colors competing in the same view.
- Heavy drop shadows (`0 12px 36px rgba(0,0,0,.55)`), glows on everything.
- Mixed corner radii on sibling elements.
- Inconsistent padding between similar panels.
- Body text in pure white (`#fff`) on pure black — use off-white on near-black.
- Uppercase microlabels with no letter-spacing, or body copy *with* heavy
  tracking. Tracking is for ≤12px uppercase labels only.
- Buttons/inputs of subtly different heights in the same row.

---

## 2. Spacing scale (4px base, 8pt rhythm)

Token → value. Use these for padding, margin, and gap. Default component
padding is `--space-3`/`--space-4`; section gaps `--space-5`/`--space-6`.

| Token | px | Typical use |
|---|---|---|
| `--space-px` | 1 | hairline insets |
| `--space-0_5` | 2 | icon nudges |
| `--space-1` | 4 | tight inline gaps |
| `--space-2` | 8 | base unit; chip padding, small gaps |
| `--space-3` | 12 | control padding (y), list-row gap |
| `--space-4` | 16 | panel/card padding, control padding (x) |
| `--space-5` | 20 | group spacing |
| `--space-6` | 24 | section padding |
| `--space-8` | 32 | major section gap |
| `--space-10` | 40 | view padding |
| `--space-12` | 48 | empty-state breathing room |
| `--space-16` | 64 | hero/empty-state vertical |

---

## 3. Type scale

Sans: **Inter** (UI). Mono: **JetBrains Mono** (code, data, usage numbers).
Body baseline is 13.5px — dense-desktop standard (Linear ≈13, Zed ≈13–14).

| Token | px | Use |
|---|---|---|
| `--text-xs` | 11 | uppercase microlabels, badges, timestamps |
| `--text-sm` | 12 | secondary text, captions, table cells |
| `--text-base` | 13.5 | body / default UI text |
| `--text-md` | 15 | emphasized body, list titles |
| `--text-lg` | 17 | panel headings |
| `--text-xl` | 20 | view titles |
| `--text-2xl` | 24 | empty-state / modal titles |
| `--text-3xl` | 30 | hero |

**Weights:** `--weight-normal` 400 (body) · `--weight-medium` 500 (labels,
buttons, list titles) · `--weight-semibold` 600 (headings) · `--weight-bold`
700 (reserve: numeric emphasis, brand).

**Line-height:** `--leading-tight` 1.25 (headings) · `--leading-snug` 1.4
(dense lists) · `--leading-normal` 1.55 (body) · `--leading-relaxed` 1.7 (long
prose / chat markdown).

**Tracking:** `--tracking-tight` -0.01em (large headings) · `--tracking-normal`
0 (body) · `--tracking-wide` 0.02em · `--tracking-caps` 0.06em (uppercase
microlabels ≤12px ONLY).

---

## 4. Radius, borders, elevation

**Radius:** `--radius-sm` 4 (chips, tags) · `--radius` 6 (buttons, inputs) ·
`--radius-md` 8 · `--radius-lg` 10 (cards, panels) · `--radius-xl` 14 (modals,
large surfaces) · `--radius-pill` 999.

**Borders:** `--border` (hairline, default) · `--border-strong` (structural
dividers, focused-input ring base). 1px. Never thicker than 1px for chrome.

**Elevation** — depth = lighter surface + hairline + soft layered shadow:
- `--bg` base → `--bg-elevated` (panels/cards) → `--bg-hover` (raised/active)
- `--shadow-sm` resting cards · `--shadow-md` dropdowns/popovers · `--shadow-lg`
  modals/command palette. Each is **two-layer** (tight contact + soft ambient)
  at low opacity. Dark themes: subtle (surface+border carry depth). Light
  themes: shadow carries most of the depth.

---

## 5. Motion

| Token | value | Use |
|---|---|---|
| `--duration-fast` | 90ms | hover/active color & bg |
| `--duration-base` | 150ms | most transitions, popovers |
| `--duration-slow` | 240ms | modal/sheet enter |
| `--ease-out` | cubic-bezier(0.16, 1, 0.3, 1) | enters (decelerate) |
| `--ease-standard` | cubic-bezier(0.2, 0, 0, 1) | general |
| `--ease-in-out` | cubic-bezier(0.4, 0, 0.2, 1) | moves both ways |

Respect `prefers-reduced-motion`: drop transforms, keep opacity.

---

## 6. Color & themes

Each theme is a **complete palette** written onto `:root` by `themes.ts`
(`applyTheme`). Tokens: `--bg / --bg-elevated / --bg-sunken / --bg-hover`,
`--border / --border-strong`, `--text / --text-dim / --text-muted`,
`--accent / --accent-strong / --accent-dim / --accent-fg` (+ `-soft/-softer/
-glow` rgba tints), `--success / --warning / --danger / --info`.

**Contrast targets:** body `--text` ≥ 7:1 on `--bg`; `--text-dim` ≥ 4.5:1;
`--text-muted` ≥ 4:1 (microlabels). Accent-on-`--accent-fg` ≥ 4.5:1.

**Flagship themes** (the ones we polish hardest):
- **Coral Dark** (default) — near-black, warm coral accent. The brand face.
- **Slate** — cool neutral dark, soft indigo. Calm tooling default.
- **Graphite** — pure-neutral charcoal, restrained teal.
- **Paper** — off-white light, indigo accent. Light mode must be flawless:
  visible-but-soft borders, AA dim/muted text, shadow-driven elevation.

Theme systems: `themes.ts` (curated, full palettes — primary) and
`themes-custom.ts` (user JSON themes + presets, subset of tokens). **Planned
consolidation:** one engine so a custom theme also gets the full token set.

---

## 7. Component contract (target states)

Adopt the tokens above. Rolling out per run.

- **Buttons** — height 28px (sm) / 32px (md); padding `--space-2`/`--space-4`;
  radius `--radius`; weight medium. *Primary*: accent bg, `--accent-fg`.
  *Secondary*: `--bg-elevated` + hairline. *Ghost*: transparent → `--bg-hover`.
  Single focus ring. `--duration-fast` color transitions.
- **Inputs / selects** — match button height; `--bg-sunken`; hairline →
  `--accent` border + soft glow on focus; placeholder `--text-muted`.
- **Cards / panels** — `--bg-elevated`, hairline, `--radius-lg`, `--space-4`
  padding, `--shadow-sm` only when truly floating.
- **Sidebar / activity bar** — `--bg-sunken`; active item = `--accent-soft` bg +
  `--accent` left marker or text; hover = `--bg-hover`. Icon + label rhythm.
- **Chat composer** — `--bg-elevated`, `--radius-lg`, hairline → accent on
  focus; send button primary. Generous internal padding (`--space-4`).
- **Message bubbles** — minimal: role label (`--text-xs` caps, `--text-muted`),
  `--leading-relaxed` markdown, code blocks `--bg-sunken` + mono.
- **Model picker / dropdowns** — `--bg-elevated`, `--shadow-md`, `--radius-md`,
  selected row `--accent-soft`. Tight 1.4 leading rows.
- **Usage dashboard** — mono numbers, `--text-2xl` figures, `--text-xs` caps
  labels, semantic colors only on threshold breach.
- **Modals / command palette** — `--bg-elevated`, `--radius-xl`, `--shadow-lg`,
  scrim `rgba(0,0,0,.5)`, enter `--duration-slow --ease-out`.
- **Tabs / chips / scrollbars** — consistent radii; scrollbars thin neutral
  pills (already done); chips `--radius-sm`, `--text-xs`.
- **Empty / loading states** — centered, `--space-12`+ padding, `--text-2xl`
  muted title + one-line hint + a single primary action. Never a blank pane.

---

## 8. References studied
Linear (spacing/density, restrained accent, focus), Raycast (layered elevation,
command palette), Zed/gpui (dense neutral dark, perf-minded chrome), Vercel
dashboard (neutral-first, hairlines, light mode), Cursor (AI-tool chat/composer
layout), Superhuman (dense rhythm, keyboard-first polish).
