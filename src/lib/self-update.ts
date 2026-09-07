import { invoke } from "@tauri-apps/api/core";

/**
 * In-app self-update against the Gitea release (Linux AppImage only). The
 * backend (`commands/selfupdate.rs`) reports `supported: false` on every other
 * platform/packaging, so callers can treat a non-AppImage build as a no-op.
 */
export interface ReleaseUpdate {
  supported: boolean;
  available: boolean;
  current_key: string | null;
  latest_key: string | null;
  tag: string | null;
  asset_name: string | null;
  download_url: string | null;
}

export async function checkReleaseUpdate(): Promise<ReleaseUpdate> {
  return invoke<ReleaseUpdate>("check_release_update");
}

/** Download + atomically swap the running AppImage. Resolves to "applied". */
export async function applyReleaseUpdate(
  downloadUrl: string,
  assetKey: string,
): Promise<string> {
  return invoke<string>("apply_release_update", { downloadUrl, assetKey });
}

/** Restart the app so a freshly-swapped AppImage takes effect. */
export async function relaunchApp(): Promise<void> {
  await invoke("relaunch_app");
}
