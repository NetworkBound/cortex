/**
 * Command palette index: the pure scoring / ranking / recents layer behind
 * `CommandPalette.tsx`, plus the handful of cross-surface actions the palette
 * and the global keymap (App.tsx) both dispatch, so the two can't drift.
 *
 * Layout of this module:
 *   1. Fuzzy scorer — subsequence match with word-start + adjacency bonuses.
 *      No React, no store; unit-testable in isolation.
 *   2. Recents — `localStorage["cortex.palette.recent"]`, capped, keyed by
 *      item id. Adds a recency weight to ranking and feeds the "Recent" group.
 *   3. Query parsing + ranking — prefix modes (`>` actions/commands, `@`
 *      threads/sessions, `#` settings, `/` slash commands) and grouping.
 *   4. Actions — theme cycling, "jump to pending approval", "new routine",
 *      settings deep-link. These touch the store/DOM and are shared with the
 *      keymap handler in App.tsx.
 */

import { countApprovals, focusOldestApproval } from "@/lib/attention";
import { findCommand, makeContext } from "@/lib/slash-commands";
import { pushToast } from "@/lib/toast";
import {
  applyCustomTheme,
  getActiveThemeState,
  loadAllThemes,
  setActiveThemeName,
} from "@/lib/themes-custom";
import { useCortexStore } from "@/state/store";
import { TABS, persistTab, type TabId } from "@/components/settings/types";

// ── 1. Items + fuzzy scorer ─────────────────────────────────────────────────

/** What kind of thing a palette row is — drives the prefix filters. */
export type PaletteKind =
  | "action"
  | "slash"
  | "nav"
  | "thread"
  | "session"
  | "settings"
  | "project"
  | "profile";

export interface PaletteItem {
  /** Stable id (also the recents key). */
  id: string;
  kind: PaletteKind;
  label: string;
  /** Right-aligned hint (shortcut, group, relative time…). */
  hint?: string;
  /** Extra searchable text not shown in the label (aliases, keywords). */
  keywords?: string;
  /** Display group header. */
  section: string;
  run: () => void | Promise<void>;
}

export interface PaletteGroup {
  section: string;
  items: PaletteItem[];
}

function isWordStart(text: string, i: number): boolean {
  if (i === 0) return true;
  const prev = text[i - 1];
  const cur = text[i];
  if (!/[a-z0-9]/i.test(prev)) return true;
  // camelCase boundary: lower → upper.
  return prev === prev.toLowerCase() && cur !== cur.toLowerCase();
}

/**
 * Score one query token against `text`. 0 = no match. Higher is better.
 * Every character of `token` must appear in order in `text`; matches at word
 * starts and runs of adjacent matches score extra, and shorter targets win
 * ties so "Go to Git" beats "Go to Git history" for the query "git".
 */
function scoreToken(token: string, text: string): number {
  if (token.length === 0) return 1;
  const lcText = text.toLowerCase();
  const idx = lcText.indexOf(token);
  let score = 0;
  if (idx >= 0) {
    // Contiguous substring: strong signal. Prefix and word-start stronger.
    score = 10 + token.length * 2;
    if (idx === 0) score += 8;
    else if (isWordStart(text, idx)) score += 5;
  } else {
    let ti = 0;
    let prevMatch = -2;
    for (let i = 0; i < lcText.length && ti < token.length; i++) {
      if (lcText[i] !== token[ti]) continue;
      if (isWordStart(text, i)) score += 4;
      else if (i === prevMatch + 1) score += 3;
      else score += 1;
      prevMatch = i;
      ti++;
    }
    if (ti < token.length) return 0;
  }
  // Mild length penalty so compact labels rank above verbose ones.
  return score - Math.min(text.length, 200) * 0.02;
}

/**
 * Fuzzy score of a whitespace-separated `query` against `text`. Each token
 * must match (subsequence) somewhere in `text`; 0 means "no match". An empty
 * query matches everything with a score of 1.
 */
export function fuzzyScore(query: string, text: string): number {
  const tokens = query.toLowerCase().trim().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return 1;
  let total = 0;
  for (const t of tokens) {
    const s = scoreToken(t, text);
    if (s <= 0) return 0;
    total += s;
  }
  return total;
}

/** Score an item: best of label / label+keywords / hint, hint discounted. */
export function scoreItem(item: PaletteItem, query: string): number {
  const label = fuzzyScore(query, item.label);
  const withKw = item.keywords
    ? fuzzyScore(query, `${item.label} ${item.keywords}`) * 0.9
    : 0;
  const hint = item.hint ? fuzzyScore(query, item.hint) * 0.6 : 0;
  return Math.max(label, withKw, hint);
}

// ── 2. Recents ──────────────────────────────────────────────────────────────

export const RECENTS_KEY = "cortex.palette.recent";
export const RECENTS_CAP = 20;
/** How many resolved recents the empty-query view shows. */
export const RECENTS_SHOWN = 6;

export interface RecentEntry {
  id: string;
  ts: number;
}

export function loadRecents(): RecentEntry[] {
  try {
    const raw = localStorage.getItem(RECENTS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (e): e is RecentEntry =>
          !!e &&
          typeof e === "object" &&
          typeof (e as RecentEntry).id === "string" &&
          typeof (e as RecentEntry).ts === "number",
      )
      .slice(0, RECENTS_CAP);
  } catch {
    return [];
  }
}

/** Pure: move `id` to the front with `ts`, drop duplicates, cap the list. */
export function pushRecent(
  list: RecentEntry[],
  id: string,
  ts: number,
): RecentEntry[] {
  return [{ id, ts }, ...list.filter((e) => e.id !== id)].slice(0, RECENTS_CAP);
}

export function recordRecent(id: string): void {
  try {
    localStorage.setItem(
      RECENTS_KEY,
      JSON.stringify(pushRecent(loadRecents(), id, Date.now())),
    );
  } catch {
    /* storage unavailable — recents are a convenience only */
  }
}

const WEEK_MS = 7 * 24 * 3600 * 1000;

/** 0..1 recency weight: 1 for "just used", decaying linearly over a week. */
export function recencyWeight(
  id: string,
  recents: RecentEntry[],
  now: number,
): number {
  const hit = recents.find((e) => e.id === id);
  if (!hit) return 0;
  const age = Math.max(0, now - hit.ts);
  return Math.max(0, 1 - age / WEEK_MS);
}

// ── 3. Query parsing + ranking ──────────────────────────────────────────────

export type PaletteMode = "all" | "commands" | "threads" | "settings" | "slash";

export interface ParsedQuery {
  mode: PaletteMode;
  text: string;
}

/** `>` actions + commands, `@` threads/sessions, `#` settings, `/` slash. */
export function parseQuery(raw: string): ParsedQuery {
  const q = raw.trimStart();
  const first = q[0];
  if (first === ">") return { mode: "commands", text: q.slice(1).trim() };
  if (first === "@") return { mode: "threads", text: q.slice(1).trim() };
  if (first === "#") return { mode: "settings", text: q.slice(1).trim() };
  if (first === "/") return { mode: "slash", text: q.slice(1).trim() };
  return { mode: "all", text: q.trim() };
}

const MODE_KINDS: Record<Exclude<PaletteMode, "all">, Set<PaletteKind>> = {
  commands: new Set(["action", "slash", "profile"]),
  threads: new Set(["thread", "session"]),
  settings: new Set(["settings"]),
  slash: new Set(["slash"]),
};

export const RECENT_SECTION = "Recent";

/** Max rows per group while a search is active — keeps noisy groups short. */
export const SEARCH_GROUP_CAP = 12;

export interface RankOptions {
  /** Group header order for the browse (empty-query) view. Unknown sections
   *  sort alphabetically after the listed ones. */
  sectionOrder: readonly string[];
  recents: RecentEntry[];
  now: number;
}

/**
 * Rank + group items for the given raw query.
 *
 * Empty query (browse): a "Recent" group first (resolved from `recents`,
 * most recent first), then every group in `sectionOrder`. Prefix modes still
 * filter by kind. Non-empty query: only matching items, groups ordered by
 * their best hit, rows ordered by score + recency, each group capped.
 */
export function rankPalette(
  items: PaletteItem[],
  rawQuery: string,
  opts: RankOptions,
): PaletteGroup[] {
  const { mode, text } = parseQuery(rawQuery);
  const pool =
    mode === "all" ? items : items.filter((i) => MODE_KINDS[mode].has(i.kind));

  const orderIndex = (section: string) => {
    const i = opts.sectionOrder.indexOf(section);
    return i < 0 ? opts.sectionOrder.length : i;
  };
  const bySection = (a: string, b: string) =>
    orderIndex(a) - orderIndex(b) || a.localeCompare(b);

  if (text.length === 0) {
    const groups: PaletteGroup[] = [];
    if (mode === "all") {
      const byId = new Map(pool.map((i) => [i.id, i]));
      const recent: PaletteItem[] = [];
      for (const r of opts.recents) {
        const it = byId.get(r.id);
        if (it) recent.push(it);
        if (recent.length >= RECENTS_SHOWN) break;
      }
      if (recent.length > 0)
        groups.push({ section: RECENT_SECTION, items: recent });
    }
    const buckets = new Map<string, PaletteItem[]>();
    for (const it of pool) {
      const arr = buckets.get(it.section) ?? [];
      arr.push(it);
      buckets.set(it.section, arr);
    }
    for (const section of [...buckets.keys()].sort(bySection)) {
      groups.push({ section, items: buckets.get(section)! });
    }
    return groups;
  }

  type Scored = { item: PaletteItem; score: number };
  const buckets = new Map<string, Scored[]>();
  for (const item of pool) {
    const base = scoreItem(item, text);
    if (base <= 0) continue;
    const score = base + 6 * recencyWeight(item.id, opts.recents, opts.now);
    const arr = buckets.get(item.section) ?? [];
    arr.push({ item, score });
    buckets.set(item.section, arr);
  }
  const groups = [...buckets.entries()].map(([section, rows]) => {
    rows.sort((a, b) => b.score - a.score);
    return {
      section,
      best: rows[0].score,
      items: rows.slice(0, SEARCH_GROUP_CAP).map((r) => r.item),
    };
  });
  groups.sort((a, b) => b.best - a.best || bySection(a.section, b.section));
  return groups.map(({ section, items }) => ({ section, items }));
}

// ── 4. Shared actions ───────────────────────────────────────────────────────

/** Ask ChatPane to put the caret back in the composer. */
export function focusComposer(): void {
  window.dispatchEvent(new CustomEvent("cortex:composer-focus"));
}

/**
 * Cycle to the next theme (built-ins + user themes, in gallery order) via the
 * same apply + persist path the Settings theme picker uses.
 */
export async function cycleTheme(): Promise<void> {
  try {
    const [all, state] = await Promise.all([
      loadAllThemes(),
      getActiveThemeState(),
    ]);
    if (all.length === 0) return;
    const idx = all.findIndex((t) => t.name === state.active);
    const next = all[(idx + 1) % all.length];
    applyCustomTheme(next);
    try {
      await setActiveThemeName(next.name);
    } catch {
      /* applied live; persistence is best-effort */
    }
    pushToast({ title: `Theme: ${next.name}`, kind: "info" });
  } catch (e) {
    console.warn("cycleTheme failed", e);
  }
}

/** Number of unanswered approvals across every thread (for the palette row). */
export function pendingApprovalCount(): number {
  return countApprovals(useCortexStore.getState().threads);
}

/**
 * Switch to the thread holding the oldest pending approval and highlight its
 * prompt. Delegates to `lib/attention` (the same path the rail badge, the
 * Today card and the `cortex:focus-approval` event use) so every entry point
 * lands on the same message. Returns false — with a small toast — when
 * nothing is waiting.
 */
export function jumpToPendingApproval(): boolean {
  return focusOldestApproval();
}

/** Retry `fn` every 50 ms until it returns true or `tries` runs out. Used to
 *  wait for a lazily-loaded panel to mount before poking its DOM. */
function pollDom(fn: () => boolean, tries = 40): void {
  const tick = () => {
    if (fn()) return;
    if (tries-- > 0) setTimeout(tick, 50);
  };
  setTimeout(tick, 0);
}

/** Open the Routines panel with its "New routine" form expanded. */
export function newRoutine(): void {
  useCortexStore.getState().setActivityTab("routines");
  pollDom(() => {
    const btn = document.querySelector<HTMLButtonElement>(".routines-new-btn");
    if (!btn) return false;
    if (!document.querySelector(".routines-form")) btn.click();
    return true;
  });
}

/** Human label for a settings tab id. */
export function settingsTabLabel(tab: TabId): string {
  return TABS.find((t) => t.id === tab)?.label ?? tab;
}

/**
 * Open Settings on `tab` and scroll `heading`'s card into view with a brief
 * highlight. The tab is persisted first so a cold SettingsModal mounts on it;
 * an already-mounted modal is steered through its nav buttons. A
 * `cortex:settings-reveal` event is dispatched too so the modal can take
 * over the steering natively when it learns to listen.
 */
export function revealSettingsSection(tab: TabId, heading: string): void {
  persistTab(tab);
  useCortexStore.getState().setShowSettings(true);
  window.dispatchEvent(
    new CustomEvent("cortex:settings-reveal", { detail: { tab, heading } }),
  );
  const label = settingsTabLabel(tab);
  const want = heading.trim().toLowerCase();
  pollDom(() => {
    const btn = [
      ...document.querySelectorAll<HTMLButtonElement>(".settings-nav-btn"),
    ].find((b) => b.textContent?.trim() === label);
    if (!btn) return false;
    if (btn.getAttribute("aria-selected") !== "true") btn.click();
    pollDom(() => {
      const h = [
        ...document.querySelectorAll<HTMLElement>(
          ".settings-content h3, .settings-content h2",
        ),
      ].find((x) => x.textContent?.trim().toLowerCase() === want);
      if (!h) return false;
      const card = h.closest<HTMLElement>(".settings-section") ?? h;
      card.scrollIntoView({ block: "start" });
      card.classList.add("palette-reveal");
      setTimeout(() => card.classList.remove("palette-reveal"), 1600);
      return true;
    }, 20);
    return true;
  });
}

/**
 * Actions for the DEFAULT_KEYMAP ids that no component owns itself. App.tsx
 * binds these globally; the palette lists the same actions with their combos
 * as hints. Ids handled elsewhere (palette, settings, shortcuts, cycle-mode,
 * send, resume, memory-search, cycle-tab) are intentionally absent so they
 * are never double-bound.
 */
export const GLOBAL_KEYMAP_ACTIONS: Record<string, () => void> = {
  "new-session": () => {
    useCortexStore.getState().resetSession();
    focusComposer();
  },
  quickopen: () => {
    void import("@/lib/quick-open").then((m) => m.openQuickOpen());
  },
  "cycle-theme": () => void cycleTheme(),
  compact: () => runSlash("compact"),
  "new-window": () => runSlash("newwindow"),
  "focus-approval": () => void jumpToPendingApproval(),
};

/** Dispatch a slash command by name through the live SlashContext. */
function runSlash(name: string): void {
  const cmd = findCommand(`/${name}`);
  if (!cmd) {
    console.warn(`slash command /${name} is not registered`);
    return;
  }
  void Promise.resolve(cmd.run("", makeContext())).catch((e) =>
    console.error(`/${name} failed`, e),
  );
}
