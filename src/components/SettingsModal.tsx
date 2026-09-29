import { useEffect, useMemo, useRef, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  getGatewayConfig,
  setGatewayApiKey,
  updateGatewayConfig,
} from "@/lib/cortex-bridge";
import { notifyGatewayConfigChanged } from "@/lib/gateway";
import { setObsidianVault } from "@/lib/brain";
import { DEFAULT_KEYMAP, matchCombo } from "@/lib/keymap";
import { useCortexStore } from "@/state/store";
import { ALL_SECTIONS } from "./settings/sections";
import { UnsavedBadge } from "./settings/Section";
import {
  TABS,
  loadTab,
  persistTab,
  type GatewayForm,
  type SectionDef,
  type SettingsCtx,
  type TabId,
} from "./settings/types";
import "@/styles/settings.css";

/**
 * Settings dialog shell: two-column (category rail + scrolling content), a
 * search box that filters sections across every tab by heading + keyword
 * index, and a footer Save for the gateway/Ollama/Obsidian form. Section
 * bodies live in `./settings/*Tab.tsx`; this file only owns the shared form,
 * the tab/search state and the keyboard wiring (Esc closes, Ctrl+, toggles,
 * focus returns to the composer on close).
 */

// Snapshot of the gateway form as loaded, so "unsaved changes" can be derived
// by comparison instead of tracked by hand on every setter.
interface GatewaySnapshot {
  baseUrl: string;
  model: string;
  ollamaUrl: string;
  ollamaModel: string;
  obsidian: string;
}

const SETTINGS_COMBO =
  DEFAULT_KEYMAP.find((b) => b.id === "settings")?.combo ?? "Ctrl+,";

export function SettingsModal() {
  const show = useCortexStore((s) => s.showSettings);
  const setShow = useCortexStore((s) => s.setShowSettings);
  const setHasApiKey = useCortexStore((s) => s.setHasApiKey);

  const [activeTab, setActiveTab] = useState<TabId>(() => loadTab());
  const [query, setQuery] = useState("");

  // Connection fields start empty ("not configured") and are hydrated from
  // the backend config when the modal opens — no baked-in addresses.
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("gateway-agent");
  const [apiKey, setApiKey] = useState("");
  const [ollamaUrl, setOllamaUrl] = useState("");
  const [ollamaModel, setOllamaModel] = useState("qwen2.5:14b");
  const [obsidian, setObsidian] = useState("");
  const [hasKey, setHasKey] = useState(false);
  const [loaded, setLoaded] = useState<GatewaySnapshot | null>(null);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!show) return;
    let cancelled = false;
    getGatewayConfig()
      .then((cfg) => {
        if (cancelled) return;
        setBaseUrl(cfg.base_url);
        setModel(cfg.model);
        setOllamaUrl(cfg.ollama_base_url);
        setOllamaModel(cfg.ollama_model);
        setHasKey(cfg.has_api_key);
        setObsidian(cfg.obsidian_vault ?? "");
        setLoaded({
          baseUrl: cfg.base_url,
          model: cfg.model,
          ollamaUrl: cfg.ollama_base_url,
          ollamaModel: cfg.ollama_model,
          obsidian: cfg.obsidian_vault ?? "",
        });
      })
      .catch((e) => {
        if (!cancelled) setErr(humanizeError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [show]);

  useEffect(() => {
    persistTab(activeTab);
  }, [activeTab]);

  // Keyboard: Esc closes (capture phase so shortcut handlers underneath never
  // see it), Ctrl+, toggles the dialog. A nested CLI sign-in terminal owns
  // Esc while it is open so the login flow isn't yanked away mid-auth.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (matchCombo(e, SETTINGS_COMBO)) {
        e.preventDefault();
        setShow(!show);
        return;
      }
      if (!show || e.key !== "Escape") return;
      if (document.querySelector(".cli-login-backdrop")) return;
      e.preventDefault();
      e.stopPropagation();
      setShow(false);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [show, setShow]);

  // Focus management: the search box takes focus on open; on close, hand
  // focus back to the chat composer so the next keystroke lands in the draft.
  const wasShown = useRef(false);
  useEffect(() => {
    if (show) {
      wasShown.current = true;
      setTimeout(() => searchRef.current?.focus(), 0);
    } else if (wasShown.current) {
      wasShown.current = false;
      window.dispatchEvent(new CustomEvent("cortex:composer-focus"));
    }
  }, [show]);

  async function save() {
    setSaving(true);
    setErr(null);
    try {
      await updateGatewayConfig({
        gateway_base_url: baseUrl,
        gateway_model: model,
        ollama_base_url: ollamaUrl,
        ollama_model: ollamaModel,
      });
      if (apiKey.trim().length > 0) {
        await setGatewayApiKey(apiKey.trim());
        setHasKey(true);
        setHasApiKey(true);
        setApiKey("");
      }
      await setObsidianVault(obsidian.trim() || null);
      // Let gateway-gated surfaces (deep research, …) re-check without a reload.
      notifyGatewayConfigChanged();
      setShow(false);
    } catch (e) {
      setErr(humanizeError(e));
    } finally {
      setSaving(false);
    }
  }

  const dirty =
    apiKey.trim().length > 0 ||
    (loaded !== null &&
      (loaded.baseUrl !== baseUrl ||
        loaded.model !== model ||
        loaded.ollamaUrl !== ollamaUrl ||
        loaded.ollamaModel !== ollamaModel ||
        loaded.obsidian !== obsidian));

  const ctx: SettingsCtx = useMemo(() => {
    const gateway: GatewayForm = {
      baseUrl,
      setBaseUrl,
      model,
      setModel,
      apiKey,
      setApiKey,
      hasKey,
      ollamaUrl,
      setOllamaUrl,
      ollamaModel,
      setOllamaModel,
      obsidian,
      setObsidian,
      dirty,
    };
    return { gateway };
  }, [baseUrl, model, apiKey, hasKey, ollamaUrl, ollamaModel, obsidian, dirty]);

  const q = query.trim().toLowerCase();
  const matches = (s: SectionDef) =>
    q.length === 0 ||
    s.heading.toLowerCase().includes(q) ||
    s.text.toLowerCase().includes(q);

  // Which tabs have any matching section under the current search? When a
  // query is active we hide tabs with zero hits and auto-pick the first
  // matching tab if the current one is empty.
  const tabHasHits = useMemo(() => {
    const map = Object.fromEntries(TABS.map((t) => [t.id, false])) as Record<
      TabId,
      boolean
    >;
    for (const s of ALL_SECTIONS) if (matches(s)) map[s.tab] = true;
    return map;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q]);

  const visibleTabs =
    q.length > 0 ? TABS.filter((t) => tabHasHits[t.id]) : TABS;

  useEffect(() => {
    if (q.length === 0) return;
    if (!tabHasHits[activeTab] && visibleTabs.length > 0) {
      setActiveTab(visibleTabs[0].id);
    }
  }, [q, tabHasHits, activeTab, visibleTabs]);

  if (!show) return null;

  const visibleSections = ALL_SECTIONS.filter(
    (s) => s.tab === activeTab && matches(s),
  );

  return (
    <div className="modal-backdrop" onClick={() => setShow(false)}>
      <div
        className="modal modal-settings"
        role="dialog"
        aria-modal="true"
        aria-labelledby="settings-title"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="settings-header">
          <h2 id="settings-title">Settings</h2>
          <button
            type="button"
            className="settings-close"
            onClick={() => setShow(false)}
            aria-label="Close settings"
            title="Close (Esc)"
          >
            ×
          </button>
        </div>
        <div className="settings-body">
          <nav className="settings-nav" aria-label="Settings categories">
            <div className="settings-search">
              <input
                ref={searchRef}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search settings…"
                aria-label="Search settings"
              />
            </div>
            <div className="settings-nav-list" role="tablist">
              {visibleTabs.length === 0 && (
                <div className="settings-nav-empty">No matches</div>
              )}
              {visibleTabs.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  role="tab"
                  aria-selected={t.id === activeTab}
                  className={`settings-nav-btn${t.id === activeTab ? " active" : ""}`}
                  onClick={() => setActiveTab(t.id)}
                >
                  {t.label}
                </button>
              ))}
            </div>
          </nav>
          <div className="settings-content" role="tabpanel">
            {visibleSections.length === 0 ? (
              <div className="settings-hint">
                {q.length > 0
                  ? "No settings match your search in this tab."
                  : "Nothing to configure in this tab yet."}
              </div>
            ) : (
              visibleSections.map((s) => (
                <div key={`${s.tab}-${s.heading}`}>{s.render(ctx)}</div>
              ))
            )}
          </div>
        </div>
        <div className="settings-footer">
          {err && <div className="settings-err">{err}</div>}
          <UnsavedBadge show={dirty} />
          <div className="modal-actions">
            <button
              type="button"
              onClick={() => setShow(false)}
              disabled={saving}
            >
              {dirty ? "Cancel" : "Close"}
            </button>
            <button
              type="button"
              className="btn-primary"
              onClick={() => void save()}
              disabled={saving}
            >
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
