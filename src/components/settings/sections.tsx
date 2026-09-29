import { SettingsThemeTab } from "../SettingsThemeTab";
import { GENERAL_SECTIONS } from "./GeneralTab";
import { CONNECTIONS_SECTIONS } from "./ConnectionsTab";
import { PROVIDERS_SECTIONS } from "./ProvidersTab";
import { WORKSPACE_SECTIONS } from "./WorkspaceTab";
import { UPDATES_SECTIONS } from "./UpdatesTab";
import { ADVANCED_SECTIONS } from "./AdvancedTab";
import type { SectionDef } from "./types";

const THEME_SECTIONS: SectionDef[] = [
  {
    tab: "theme",
    heading: "Appearance",
    text: "theme appearance dark light zinc amber carbon solarized accent palette background image wallpaper customize colors",
    render: () => <SettingsThemeTab />,
  },
];

/** Every settings section, in tab order. Searched and rendered by SettingsModal. */
export const ALL_SECTIONS: SectionDef[] = [
  ...GENERAL_SECTIONS,
  ...CONNECTIONS_SECTIONS,
  ...PROVIDERS_SECTIONS,
  ...WORKSPACE_SECTIONS,
  ...THEME_SECTIONS,
  ...UPDATES_SECTIONS,
  ...ADVANCED_SECTIONS,
];

/** Searchable metadata for every section (no render functions) — what the
 *  command palette indexes for "Settings › Tab › Heading" deep links. */
export interface SettingsSectionMeta {
  tab: SectionDef["tab"];
  heading: string;
  text: string;
}

export const SETTINGS_SECTION_INDEX: SettingsSectionMeta[] = ALL_SECTIONS.map(
  ({ tab, heading, text }) => ({ tab, heading, text }),
);
