import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { humanizeError } from "@/lib/errors";
import {
  checkUpdates,
  configuredManifestUrl,
  type UpdateInfo,
} from "@/lib/updater";
import { SettingsSection } from "./Section";
import type { SectionDef } from "./types";

// Running build version, cached for the app lifetime (it can't change without
// a restart) so reopening the tab doesn't flash "—" before the async read.
let appVersionMemo: string | null = null;

/**
 * Updates tab — manual "check for updates" flow. We never download/apply
 * here; we only surface current vs latest and link the release.
 */
function UpdatesSection() {
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(appVersionMemo);

  useEffect(() => {
    if (appVersion) return;
    let cancelled = false;
    getVersion()
      .then((v) => {
        appVersionMemo = v;
        if (!cancelled) setAppVersion(v);
      })
      .catch(() => {
        /* non-Tauri/web preview — leave version unknown rather than throw */
      });
    return () => {
      cancelled = true;
    };
  }, [appVersion]);

  async function runUpdateCheck() {
    setChecking(true);
    setErr(null);
    try {
      const manifestUrl = configuredManifestUrl();
      if (!manifestUrl) {
        // No baked-in manifest URL ships with the app — humanize instead of
        // dialing anything.
        setErr(
          "No update manifest configured. Set an https:// manifest URL in localStorage under cortex.updateUrl to enable update checks.",
        );
        return;
      }
      setUpdateInfo(await checkUpdates(manifestUrl));
    } catch (e) {
      // Fetch can fail (offline, manifest unreachable). Show inline, never throw.
      setErr(humanizeError(e));
    } finally {
      setChecking(false);
    }
  }

  return (
    <SettingsSection title="Updates">
      <div className="settings-row spaced">
        <span className="settings-microlabel">Installed version</span>
        <code className="settings-emph">
          {appVersion ? `v${appVersion}` : "—"}
        </code>
      </div>
      <div className="settings-hint spaced">
        Compares your running build against the latest published release on your
        Gitea (or a manifest URL set in <code>cortex.updateUrl</code>). Newer
        builds are downloaded and installed manually — this checks and links the
        release.
      </div>
      <div className="settings-row spaced">
        <button
          type="button"
          onClick={() => void runUpdateCheck()}
          disabled={checking}
        >
          {checking ? "Checking…" : "Check for updates"}
        </button>
      </div>
      {err && (
        <div className="settings-err spaced">
          Couldn't check for updates: {err}
        </div>
      )}
      {updateInfo && (
        <div className="settings-stack">
          <div className="settings-hint">
            Current: <code className="settings-emph">{updateInfo.current}</code>
            {"  "}·{"  "}Latest:{" "}
            <code className="settings-emph">{updateInfo.latest}</code>
          </div>
          <div
            className={`settings-update-status ${updateInfo.available ? "available" : "ok"}`}
          >
            {updateInfo.available ? "↑ Update available" : "✓ Up to date"}
          </div>
          {updateInfo.notes && (
            <div className="settings-note">{updateInfo.notes}</div>
          )}
          {updateInfo.url && (
            <a
              href={updateInfo.url}
              target="_blank"
              rel="noreferrer"
              className="settings-link"
            >
              Open release →
            </a>
          )}
        </div>
      )}
    </SettingsSection>
  );
}

export const UPDATES_SECTIONS: SectionDef[] = [
  {
    tab: "updates",
    heading: "Updates",
    text: "updates version cortex tauri release auto-update check manifest latest current offline",
    render: () => <UpdatesSection />,
  },
];
