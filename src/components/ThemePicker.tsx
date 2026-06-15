/**
 * ThemePicker
 *
 * Visual grid of theme presets + a "background image" row underneath. Picking
 * a theme writes the active name through the Rust backend AND applies it to
 * `:root` immediately, so the change is visible before the round-trip
 * finishes.
 *
 * Theme tiles are live mini-previews built from the same color tokens the
 * app uses, so the user sees roughly what the running UI will look like
 * before committing.
 */
import { useCallback, useEffect, useState } from "react";
import { humanizeError } from "@/lib/errors";
import {
  applyCustomTheme,
  getActiveThemeState,
  loadAllThemes,
  setActiveThemeName,
  type Theme,
} from "../lib/themes-custom";
import {
  bgImageAssetUrl,
  clearBgImage,
  pickAndSetBgImage,
} from "../lib/bg-image";

interface ThemePickerProps {
  /** Fires after either the theme or the background image changes. */
  onChange?: () => void;
}

export function ThemePicker({ onChange }: ThemePickerProps) {
  const [themes, setThemes] = useState<Theme[]>([]);
  const [activeName, setActiveName] = useState<string>("");
  const [bgUrl, setBgUrl] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [all, state] = await Promise.all([
        loadAllThemes(),
        getActiveThemeState(),
      ]);
      setThemes(all);
      setActiveName(state.active);
      setBgUrl(bgImageAssetUrl(state.bg_image_path));
    } catch (e) {
      setError(humanizeError(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const handlePick = useCallback(
    async (theme: Theme) => {
      setBusy(true);
      setError(null);
      try {
        // Apply locally first so the user sees the change instantly. The
        // backend write is best-effort persistence on top.
        applyCustomTheme(theme);
        await setActiveThemeName(theme.name);
        setActiveName(theme.name);
        onChange?.();
      } catch (e) {
        setError(humanizeError(e));
      } finally {
        setBusy(false);
      }
    },
    [onChange],
  );

  const handlePickBg = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await pickAndSetBgImage();
      if (next) {
        setBgUrl(bgImageAssetUrl(next.bg_image_path));
        onChange?.();
      }
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [onChange]);

  const handleClearBg = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await clearBgImage();
      setBgUrl(null);
      onChange?.();
    } catch (e) {
      setError(humanizeError(e));
    } finally {
      setBusy(false);
    }
  }, [onChange]);

  return (
    <div className="theme-picker">
      <div className="theme-grid">
        {themes.map((t) => (
          <ThemeTile
            key={t.name}
            theme={t}
            active={t.name === activeName}
            disabled={busy}
            onSelect={() => handlePick(t)}
          />
        ))}
      </div>

      <div className="theme-bg-row">
        <div className="theme-bg-preview">
          {bgUrl ? (
            <img
              src={bgUrl}
              alt="Background"
              className="theme-bg-thumb"
            />
          ) : (
            <div className="theme-bg-thumb theme-bg-thumb-empty">No image</div>
          )}
        </div>
        <div className="theme-bg-actions">
          <button
            type="button"
            onClick={handlePickBg}
            disabled={busy}
            className="btn-primary"
          >
            Choose background image…
          </button>
          <button
            type="button"
            onClick={handleClearBg}
            disabled={busy || !bgUrl}
            className="btn-ghost"
          >
            Clear background
          </button>
        </div>
      </div>

      {error ? <div className="theme-error">{error}</div> : null}
    </div>
  );
}

interface ThemeTileProps {
  theme: Theme;
  active: boolean;
  disabled: boolean;
  onSelect: () => void;
}

/**
 * Mini-preview card. Renders directly from the theme's color tokens so the
 * preview color-shifts as the user edits/swaps presets — no second source
 * of truth for what each theme "looks like".
 */
function ThemeTile({ theme, active, disabled, onSelect }: ThemeTileProps) {
  return (
    <button
      type="button"
      onClick={onSelect}
      disabled={disabled}
      className={`theme-tile${active ? " theme-tile-active" : ""}`}
      style={{
        background: theme.bg,
        color: theme.text,
        borderColor: active ? theme.accent : theme.bgElevated,
      }}
      title={theme.name}
    >
      <div className="theme-tile-header" style={{ color: theme.textDim }}>
        {theme.name}
      </div>
      <div className="theme-tile-swatches">
        <span
          className="theme-tile-swatch"
          style={{ background: theme.accent }}
          aria-label="accent"
        />
        <span
          className="theme-tile-swatch"
          style={{ background: theme.bgElevated }}
          aria-label="elevated"
        />
        <span
          className="theme-tile-swatch"
          style={{ background: theme.bgSunken }}
          aria-label="sunken"
        />
        <span
          className="theme-tile-swatch"
          style={{ background: theme.success }}
          aria-label="success"
        />
        <span
          className="theme-tile-swatch"
          style={{ background: theme.danger }}
          aria-label="danger"
        />
      </div>
      <div
        className="theme-tile-line"
        style={{ background: theme.bgElevated, color: theme.text }}
      >
        Aa <span style={{ color: theme.accent }}>accent</span>
      </div>
    </button>
  );
}
