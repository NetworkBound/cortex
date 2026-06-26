import { invoke } from "@tauri-apps/api/core";

/**
 * Ask the gateway for a Conventional Commits-style message summarising the
 * staged diff at `projectRoot`. Falls back to the unstaged diff on the
 * backend when nothing is staged.
 *
 * Throws when:
 * - `projectRoot` isn't a directory,
 * - there are no changes to summarise,
 * - the gateway call times out or returns an empty message.
 */
export async function suggestCommitMessage(projectRoot: string): Promise<string> {
  return invoke<string>("suggest_commit_message", { projectRoot });
}
