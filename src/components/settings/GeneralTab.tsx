import { useCortexStore } from "@/state/store";
import { playSound } from "@/lib/sounds";
import { PushNotifySettings } from "../PushNotifySettings";
import { SettingsSection, SettingsToggle } from "./Section";
import type { SectionDef } from "./types";

function WelcomeSection() {
  return (
    <SettingsSection
      title="Welcome"
      description="Cortex is the desktop client to your Cortex Gateway. Use the tabs on the left to configure connections, your Obsidian workspace, and other options."
    />
  );
}

/** Standalone so it subscribes to the store — a plain closure would go stale. */
function SoundsSection() {
  const soundsEnabled = useCortexStore((s) => s.soundsEnabled);
  const setSoundsEnabled = useCortexStore((s) => s.setSoundsEnabled);
  return (
    <SettingsSection title="Sounds">
      <SettingsToggle
        checked={soundsEnabled}
        onChange={(next) => {
          setSoundsEnabled(next);
          // Preview the tone immediately when toggling on so the user knows
          // what they just signed up for. Toggle-off stays silent.
          if (next) playSound("done");
        }}
        label="Enable subtle audio feedback"
        description="Short tones on completion, approval, errors, and copy/pin. Off by default."
      />
    </SettingsSection>
  );
}

export const GENERAL_SECTIONS: SectionDef[] = [
  {
    tab: "general",
    heading: "Welcome",
    text: "welcome cortex desktop gateway obsidian onboarding intro theme",
    render: () => <WelcomeSection />,
  },
  {
    tab: "general",
    heading: "Sounds",
    text: "sounds audio feedback chime done approve error tick mute beep",
    render: () => <SoundsSection />,
  },
  {
    tab: "general",
    heading: "Phone push",
    text: "phone push notifications ntfy gotify mobile approval tailscale token",
    render: () => <PushNotifySettings />,
  },
];
