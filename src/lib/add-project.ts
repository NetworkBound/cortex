/**
 * One-click "Add a project folder" — the single flow every surface uses
 * (Projects sidebar empty state + header "+", the onboarding wizard's "Pick a
 * project" step, palette entries).
 *
 * Reuses exactly what Setup's "Connect existing repo → Open project" does:
 *   1. `set_git_server_cloned_path(path)` validates the folder (must exist and
 *      contain a `.git` dir or file), registers it as a project and emits
 *      `projects:changed`, returning the canonical path.
 *   2. `list_projects` is re-fetched into the store and the matching code
 *      project is activated through `openCodeProject` (backend switch, store
 *      update, chat-session bootstrap).
 *
 * Unlike `openProjectByPath` this does NOT force the Projects tab open unless
 * asked — the wizard wants to stay on its own card.
 *
 * Folder-vs-root matching tolerates the shapes Windows canonicalization
 * produces (`\\?\C:\…` verbatim prefix, case differences, trailing
 * separators) so a freshly registered repo is found on the first try.
 */

import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { setGitServerClonedPath } from "@/lib/cortex-bridge";
import { humanizeError } from "@/lib/errors";
import { openCodeProject } from "@/lib/open-project";
import { listProjects, type ProjectMeta } from "@/lib/projects";
import { pushToast } from "@/lib/toast";
import { useCortexStore } from "@/state/store";

export interface AddProjectOptions {
  /** Switch the activity panel to the Projects tab after opening (default
   *  true — the sidebar surfaces want the new row visible; the wizard
   *  passes false). */
  revealProjects?: boolean;
  /** Native picker title. */
  title?: string;
  /** Toast failures (default true). The wizard renders errors inline. */
  toastErrors?: boolean;
}

export type AddProjectResult =
  | { status: "cancelled" }
  | { status: "opened"; project: ProjectMeta }
  | { status: "error"; message: string };

/** Windows-shaped path: drive letter or UNC / verbatim prefix. */
function looksWindows(p: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(p) || p.startsWith("\\\\");
}

/** Comparison key for a filesystem root: strips the `\\?\` verbatim prefix
 *  Rust's `canonicalize` adds on Windows, trailing separators, and case on
 *  Windows paths. Never used for display — the original string is kept. */
export function projectRootKey(p: string): string {
  let s = p;
  if (s.startsWith("\\\\?\\UNC\\")) s = "\\\\" + s.slice(8);
  else if (s.startsWith("\\\\?\\")) s = s.slice(4);
  const win = looksWindows(s);
  if (win) s = s.replace(/\//g, "\\");
  s = s.replace(/[\\/]+$/, "");
  return win ? s.toLowerCase() : s;
}

/** Turn the backend's rejection into a sentence with a next step. */
function explainAddError(e: unknown, path: string): string {
  const raw = humanizeError(e);
  if (/not a git repository/i.test(raw)) {
    return `${path} isn't a git repository. Cortex lists git projects only — run \`git init\` in that folder (or clone into it from the Setup tab), then add it again.`;
  }
  if (/does not exist/i.test(raw)) {
    return `${path} doesn't exist or isn't a folder.`;
  }
  return raw;
}

/** Register `path` as a project and make it active. */
export async function addProjectByPath(
  path: string,
  opts: AddProjectOptions = {},
): Promise<AddProjectResult> {
  const trimmed = path.trim();
  if (!trimmed) return { status: "cancelled" };
  let canonical: string;
  try {
    canonical = await setGitServerClonedPath(trimmed);
  } catch (e) {
    return { status: "error", message: explainAddError(e, trimmed) };
  }
  try {
    const projects = await listProjects();
    useCortexStore.getState().setProjects(projects);
    const want = new Set([projectRootKey(canonical), projectRootKey(trimmed)]);
    const p = projects.find(
      (x) => x.kind === "code" && want.has(projectRootKey(x.root)),
    );
    if (!p) {
      return {
        status: "error",
        message: `${canonical} was registered but didn't show up in the project list — try the Projects sidebar's refresh.`,
      };
    }
    await openCodeProject(p);
    if (opts.revealProjects !== false) {
      useCortexStore.getState().setActivityTab("projects");
    }
    return { status: "opened", project: p };
  } catch (e) {
    return { status: "error", message: humanizeError(e) };
  }
}

/** Native folder picker → `addProjectByPath`. Resolves `cancelled` when the
 *  user dismisses the dialog. Failures are toasted unless `toastErrors` is
 *  false; the result is returned either way so callers can render inline. */
export async function addProjectViaDialog(
  opts: AddProjectOptions = {},
): Promise<AddProjectResult> {
  let selected: string | string[] | null;
  try {
    selected = await openDialog({
      directory: true,
      multiple: false,
      title: opts.title ?? "Add a project folder",
    });
  } catch (e) {
    const message = humanizeError(e);
    if (opts.toastErrors !== false) {
      pushToast({
        title: "Couldn't open folder picker",
        body: message,
        kind: "error",
      });
    }
    return { status: "error", message };
  }
  const path = Array.isArray(selected) ? selected[0] : selected;
  if (typeof path !== "string" || path.length === 0) {
    return { status: "cancelled" };
  }
  const result = await addProjectByPath(path, opts);
  if (result.status === "error" && opts.toastErrors !== false) {
    pushToast({
      title: "Couldn't add project",
      body: result.message,
      kind: "error",
    });
  }
  return result;
}
