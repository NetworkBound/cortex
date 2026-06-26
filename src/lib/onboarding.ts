/**
 * Onboarding tour state — keyed off the `onboardingComplete` flag in the
 * Zustand store and a parallel "force show" flag so `/tour` can re-launch it
 * even after the user has completed the original first-run flow.
 *
 * The OnboardingWizard component owns the *initial* setup wizard (vault,
 * gateway, theme); this module owns the lightweight 5-step *feature tour*
 * rendered by `OnboardingTour.tsx`. They both share the `onboardingComplete`
 * flag so once either marks it true, the wizard stops auto-mounting.
 */

const FORCE_SHOW_KEY = "cortex.tour.forceShow";

/** Listeners for the "show the tour" pulse — populated by OnboardingTour. */
type Listener = () => void;
const listeners = new Set<Listener>();

export function onTourTrigger(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Fire from `/tour` slash command to relaunch the tour regardless of state. */
export function triggerTour(): void {
  try {
    localStorage.setItem(FORCE_SHOW_KEY, "true");
  } catch {
    /* private-mode — listeners still wake up below. */
  }
  for (const fn of listeners) {
    try { fn(); } catch { /* ignore listener errors */ }
  }
}

export function consumeForceShow(): boolean {
  try {
    const v = localStorage.getItem(FORCE_SHOW_KEY) === "true";
    if (v) localStorage.removeItem(FORCE_SHOW_KEY);
    return v;
  } catch {
    return false;
  }
}

/** Static content for the 5-step feature tour. Order is meaningful. */
export interface TourStep {
  title: string;
  body: string;
  hint?: string;
}

export const TOUR_STEPS: TourStep[] = [
  {
    title: "Welcome to Cortex",
    body: "Cortex is your local-first AI workspace. Press Ctrl+K to open the command palette — your jump-anywhere command launcher.",
    hint: "Ctrl+K",
  },
  {
    title: "Memory tab",
    body: "Open the Memory panel on the right to search across your notes, Obsidian vault, and prior Claude chat history. Ctrl+Shift+F focuses the search.",
    hint: "Ctrl+Shift+F",
  },
  {
    title: "ACT vs PLAN mode",
    body: "Ctrl+M toggles between ACT and PLAN. In PLAN mode the agent thinks but cannot run write/edit tools — perfect for safe brainstorming.",
    hint: "Ctrl+M",
  },
  {
    title: "Fetch a webpage",
    body: "Type `/web https://example.com` to fetch any URL and inject its content as markdown into the chat. Works for docs, blog posts, and API references.",
    hint: "/web <url>",
  },
  {
    title: "Pick a theme",
    body: "Settings → Theme lets you swap the default zinc-amber palette for carbon, solarized, or a custom theme you author yourself.",
    hint: "Settings → Theme",
  },
];
