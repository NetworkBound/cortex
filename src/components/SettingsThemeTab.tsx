/**
 * SettingsThemeTab
 *
 * Standalone tab body for the eventual Settings → Appearance section. Not
 * yet mounted inside `SettingsModal` — drop it into the modal's tab switch
 * (see the README in this PR or the agent handoff note) when you're ready.
 *
 * Keeping the wrapper deliberately tiny: a header + the picker. The picker
 * itself handles all state and the backend round-trip.
 */
import { ThemePicker } from "./ThemePicker";

export function SettingsThemeTab() {
  return (
    <div className="settings-theme-tab">
      <div className="settings-section-header">
        <h3>Appearance</h3>
        <p className="settings-section-hint">
          Pick a theme or drop in a background image. Custom themes live in
          <code> ~/.cortex/themes/</code> — duplicate a preset and edit the
          JSON to roll your own.
        </p>
      </div>
      <ThemePicker />
    </div>
  );
}
