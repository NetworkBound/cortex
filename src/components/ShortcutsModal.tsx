import { useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { comboParts, DEFAULT_KEYMAP } from "@/lib/keymap";
import { COMMANDS } from "@/lib/slash-commands";
import { Button } from "./Button";

interface Props {
  open: boolean;
  onClose: () => void;
}

/**
 * Gestures that aren't key combos (so they can't live in DEFAULT_KEYMAP).
 * Everything with a modifier chord is rendered from the keymap itself, so the
 * cheat sheet can't list a shortcut that nothing binds.
 */
const GESTURES: { combo: string[]; label: string }[] = [
  {
    combo: ["@"],
    label:
      "Open the @-vocab picker (files/folders/symbols/git/recent/docs/memory/threads/diag/snippets/diff/problems/terminal)",
  },
  {
    combo: ["#", "snippet:name"],
    label: "Inline a saved snippet (expanded on send)",
  },
  {
    combo: ["paste / drop"],
    label: "Image into chat input → vision attachment chip",
  },
  { combo: ["Esc"], label: "Close any open modal, picker, or detail pane" },
];

/** Render a command + its aliases as one space-separated `/foo  /bar` string. */
function renderCmdLabel(
  name: string,
  aliases: string[] | undefined,
  usage: string | undefined,
): string {
  const names = [name, ...(aliases ?? [])].map((n) => `/${n}`).join("  ");
  return usage ? `${names} ${usage}` : names;
}

export function ShortcutsModal({ open, onClose }: Props) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;
  // Platform-correct tokens (Ctrl on Windows/Linux, ⌘ on macOS), straight
  // from DEFAULT_KEYMAP so this list and the bindings share one source.
  const shortcuts = [
    ...DEFAULT_KEYMAP.map((b) => ({
      key: b.id,
      combo: comboParts(b.combo),
      label: b.description,
    })),
    ...GESTURES.map((g) => ({
      key: g.combo.join("+"),
      combo: g.combo,
      label: g.label,
    })),
  ];
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal shortcuts-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <h2>Keyboard shortcuts</h2>
        <ul className="shortcuts-list">
          {shortcuts.map((s) => (
            <li key={s.key}>
              <div className="shortcut-combo">
                {s.combo.map((k, i) => (
                  <kbd key={i}>{k}</kbd>
                ))}
              </div>
              <span className="shortcut-label">{s.label}</span>
            </li>
          ))}
        </ul>
        <h2 style={{ marginTop: 14, fontSize: 17 }}>Slash commands</h2>
        <ul className="shortcuts-list">
          {COMMANDS.map((c) => (
            <li key={c.name}>
              <div className="shortcut-combo">
                <code>{renderCmdLabel(c.name, c.aliases, c.usage)}</code>
              </div>
              <span className="shortcut-label">{c.description}</span>
            </li>
          ))}
        </ul>
        <div className="modal-actions">
          <Button variant="secondary" onClick={onClose}>
            Close
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * Imperative summoner used by the `/shortcuts` slash command. Same detached-
 * root portal pattern as `openChangelogModal` (ChangelogModal.tsx) so the
 * command can pop the cheat sheet without any App.tsx wiring.
 */
let activeRoot: Root | null = null;

export function openShortcutsModal(): void {
  if (activeRoot) return; // already open
  const container = document.createElement("div");
  container.dataset.cortexMount = "shortcuts";
  document.body.appendChild(container);
  const root = createRoot(container);
  activeRoot = root;

  const close = () => {
    if (activeRoot === root) {
      activeRoot = null;
    }
    root.unmount();
    if (container.parentNode) container.parentNode.removeChild(container);
  };
  root.render(<ShortcutsModal open onClose={close} />);
}
