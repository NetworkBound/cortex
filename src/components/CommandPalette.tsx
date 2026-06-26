import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useCortexStore } from "@/state/store";
import { ACTIVITY_TABS } from "@/lib/activity-tabs";
import { applyProfile, listProfiles, type Profile } from "@/lib/profiles";
import {
  COMMANDS,
  CATEGORY_ORDER,
  categorize,
  makeContext,
  type SlashCommand,
} from "@/lib/slash-commands";

interface Command {
  id: string;
  label: string;
  hint?: string;
  category: string;
  run: () => void;
}

export function CommandPalette() {
  const open = useCortexStore((s) => s.showCommandPalette);
  const setOpen = useCortexStore((s) => s.setShowCommandPalette);
  const setShowSettings = useCortexStore((s) => s.setShowSettings);
  const setActivityTab = useCortexStore((s) => s.setActivityTab);
  const resetSession = useCortexStore((s) => s.resetSession);
  const projects = useCortexStore((s) => s.projects);
  const activeProject = useCortexStore((s) => s.activeProject);
  const setActive = useCortexStore((s) => s.setActiveProject);
  const setCurrentProfile = useCortexStore((s) => s.setCurrentProfile);
  const currentProfile = useCortexStore((s) => s.currentProfile);
  const [q, setQ] = useState("");
  const [idx, setIdx] = useState(0);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  // Collapsed categories are tracked by name. Empty set ⇒ all expanded
  // (the spec's default state). Header click toggles membership.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  // Refresh the profile list every time the palette is opened against the
  // current project. Cheap (one fs read of `<root>/.cortex/profiles/`) and
  // avoids stale data after editing a TOML on disk.
  useEffect(() => {
    if (!open) return;
    const root = activeProject?.root;
    if (!root) { setProfiles([]); return; }
    let cancelled = false;
    listProfiles(root)
      .then((list) => { if (!cancelled) setProfiles(list); })
      .catch(() => { if (!cancelled) setProfiles([]); });
    return () => { cancelled = true; };
  }, [open, activeProject?.root]);

  const commands = useMemo<Command[]>(() => {
    const c: Command[] = [
      {
        id: "settings",
        label: "Open settings",
        hint: "Cmd+,",
        category: "Cortex",
        run: () => { setShowSettings(true); setOpen(false); },
      },
      {
        id: "new-chat",
        label: "New chat session",
        hint: "Cmd+N",
        category: "Cortex",
        run: () => { resetSession(); setOpen(false); },
      },
    ];
    // Every activity surface gets a "Go to …" entry so the full nav is
    // discoverable from Ctrl+K, not just the rail. Labels/order come from the
    // single tab registry (lib/activity-tabs), so this list can't drift.
    for (const t of ACTIVITY_TABS) {
      c.push({
        id: `tab-${t.id}`,
        label: `Go to ${t.title}`,
        hint: t.group,
        category: "Go to",
        run: () => { setActivityTab(t.id); setOpen(false); },
      });
    }
    for (const p of projects) {
      c.push({
        id: `pj-${p.root}`,
        label: `Switch project → ${p.name}`,
        hint: p.has_git ? "git" : "",
        category: "Project",
        run: () => { setActive(p); setOpen(false); },
      });
    }
    const root = activeProject?.root;
    if (root) {
      for (const prof of profiles) {
        const isActive = currentProfile?.name === prof.name;
        c.push({
          id: `profile-${prof.name}`,
          label: `Profile: ${prof.name}${isActive ? " ✓" : ""}`,
          hint: prof.sandbox_tier ?? prof.model ?? "",
          category: "Workflow",
          run: () => {
            void applyProfile(root, prof.name)
              .then((p) => setCurrentProfile(p))
              .catch((e) => console.error("apply_profile failed", e));
            setOpen(false);
          },
        });
      }
    }
    // Slash commands — one palette entry per canonical name. Aliases are
    // intentionally collapsed (they all dispatch to the same `run`); listing
    // them separately would just dilute search.
    for (const sc of COMMANDS as SlashCommand[]) {
      const cat = sc.category ?? categorize(sc.name);
      c.push({
        id: `slash-${sc.name}`,
        label: `/${sc.name}${sc.usage ? ` ${sc.usage}` : ""} — ${sc.description}`,
        hint: sc.aliases && sc.aliases.length > 0 ? sc.aliases.map((a) => `/${a}`).join(" ") : "",
        category: cat,
        run: () => {
          // Dispatch through the same SlashContext the chat input uses so
          // tab-switches, modal portals, and toasts all fire identically.
          void Promise.resolve(sc.run("", makeContext())).catch((e) =>
            console.error(`/${sc.name} failed`, e),
          );
          setOpen(false);
        },
      });
    }
    return c;
  }, [projects, activeProject, profiles, currentProfile, setShowSettings, setActivityTab, setOpen, resetSession, setActive, setCurrentProfile]);

  const filtered = useMemo(() => {
    if (!q.trim()) return commands;
    const lc = q.toLowerCase();
    return commands.filter((c) => c.label.toLowerCase().includes(lc) || c.hint?.toLowerCase().includes(lc));
  }, [q, commands]);

  // Group filtered commands by category, preserving CATEGORY_ORDER. Empty
  // categories are dropped so an aggressive search query collapses the
  // header list down to just the matches. While searching, every visible
  // category is forced-open so the user always sees the hits.
  const grouped = useMemo<{ category: string; items: Command[] }[]>(() => {
    const buckets = new Map<string, Command[]>();
    for (const c of filtered) {
      const arr = buckets.get(c.category) ?? [];
      arr.push(c);
      buckets.set(c.category, arr);
    }
    const ordered: { category: string; items: Command[] }[] = [];
    for (const cat of CATEGORY_ORDER) {
      const items = buckets.get(cat);
      if (items && items.length > 0) ordered.push({ category: cat, items });
      buckets.delete(cat);
    }
    // Any category not in CATEGORY_ORDER (e.g. a future custom one) falls
    // into a trailing alpha-sorted tail so it still surfaces.
    for (const cat of [...buckets.keys()].sort()) {
      const items = buckets.get(cat)!;
      if (items.length > 0) ordered.push({ category: cat, items });
    }
    return ordered;
  }, [filtered]);

  // Flatten the visible (non-collapsed) commands in render order — drives
  // arrow-key navigation. When a search is active we ignore `collapsed`
  // so all matches stay reachable via keyboard.
  const visible = useMemo<Command[]>(() => {
    const searching = q.trim().length > 0;
    const out: Command[] = [];
    for (const g of grouped) {
      if (!searching && collapsed.has(g.category)) continue;
      out.push(...g.items);
    }
    return out;
  }, [grouped, collapsed, q]);

  // Reset highlight whenever the visible set changes shape (search, collapse).
  useEffect(() => { setIdx(0); }, [q, collapsed]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen(!open);
        setQ("");
        setIdx(0);
      } else if (e.key === "Escape" && open) {
        setOpen(false);
      } else if (open && e.key === "ArrowDown") {
        e.preventDefault();
        setIdx((i) => Math.min(i + 1, visible.length - 1));
      } else if (open && e.key === "ArrowUp") {
        e.preventDefault();
        setIdx((i) => Math.max(i - 1, 0));
      } else if (open && e.key === "Enter") {
        e.preventDefault();
        visible[idx]?.run();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, setOpen, visible, idx]);

  if (!open) return null;

  const searching = q.trim().length > 0;
  const toggleCategory = (cat: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat); else next.add(cat);
      return next;
    });
  };

  // Build a flat-index lookup so the per-group render can highlight the
  // single active row across the whole list.
  let runningIndex = 0;

  return (
    <div className="palette-backdrop" onClick={() => setOpen(false)}>
      <div className="palette" onClick={(e) => e.stopPropagation()}>
        <input
          autoFocus
          value={q}
          onChange={(e) => { setQ(e.target.value); setIdx(0); }}
          placeholder="Search commands and projects…"
        />
        <ul>
          {grouped.length === 0 && <li className="muted">no matches</li>}
          {grouped.map((g) => {
            const isCollapsed = !searching && collapsed.has(g.category);
            return (
              <li key={`group-${g.category}`} className="palette-group">
                <button
                  type="button"
                  className="palette-category"
                  onClick={() => toggleCategory(g.category)}
                  aria-expanded={!isCollapsed}
                >
                  <span className="palette-category-caret">
                    {isCollapsed ? <ChevronRight size={14} strokeWidth={1.75} /> : <ChevronDown size={14} strokeWidth={1.75} />}
                  </span>
                  <span className="palette-category-name">{g.category}</span>
                  <span className="palette-category-count">{g.items.length}</span>
                </button>
                {!isCollapsed && (
                  <ul className="palette-category-items">
                    {g.items.map((c) => {
                      const i = runningIndex++;
                      return (
                        <li
                          key={c.id}
                          className={i === idx ? "active" : ""}
                          onMouseEnter={() => setIdx(i)}
                          onClick={c.run}
                        >
                          <span>{c.label}</span>
                          {c.hint && <span className="palette-hint">{c.hint}</span>}
                        </li>
                      );
                    })}
                  </ul>
                )}
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}
