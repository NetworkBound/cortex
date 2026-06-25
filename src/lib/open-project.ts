/**
 * Shared "make this the active project" flow, extracted from ProjectSidebar so
 * every surface that hands off into a project — the sidebar rows, Setup's
 * "Open project" button after a clone, future palette entries — runs the exact
 * same path: backend `set_active_project`, store update, and chat session
 * bootstrap (CLAUDE.md / runbooks / memory + prior-session replay).
 */
import { listProjects, setActiveProject, type ProjectMeta } from "./projects";
import { bootstrapProjectSession } from "./sessions";
import { pushToast } from "./toast";
import { useCortexStore } from "@/state/store";

/** Activate a code project (kind === "code"): backend switch, store update,
 *  session bootstrap. Vault-note "projects" are files and follow a different
 *  path — see ProjectSidebar's pickVault. */
export async function openCodeProject(p: ProjectMeta): Promise<void> {
  const store = useCortexStore.getState();
  await setActiveProject(p.root);
  store.setActiveProject(p);
  // Bootstrap the chat with project context — backend loads CLAUDE.md /
  // runbooks / memory + the prior session's messages (if any). CRITICAL:
  // adopt the backend's `session_id` so subsequent `chat_send` calls use
  // the project-scoped session; the bootstrap-loaded context messages
  // belong to THAT session, not the previous global one. Reset the
  // in-memory message list to whatever bootstrap returned.
  try {
    const boot = await bootstrapProjectSession(p.root);
    if (boot?.session_id) {
      const replayed = (boot.messages ?? []).map((m) => ({
        id: m.id || `boot-${crypto.randomUUID()}`,
        role: m.role,
        content: m.content,
        agent: m.agent_id ?? undefined,
        tools: [],
      }));
      // Route through the active thread so the thread record AND the legacy
      // top-level mirrors stay in lock-step. A bare setState here writes only
      // the mirrors, so the next appendMessage re-derives them from the stale
      // thread and clobbers the adopted session — which is exactly why the
      // first message of a freshly-bootstrapped project chat wouldn't send.
      useCortexStore.getState().adoptSession({
        sessionId: boot.session_id,
        messages: replayed,
      });
    } else if (boot?.messages?.length) {
      for (const m of boot.messages) {
        useCortexStore.getState().appendMessage({
          id: m.id || `boot-${crypto.randomUUID()}`,
          role: m.role,
          content: m.content,
          agent: m.agent_id ?? undefined,
          tools: [],
        });
      }
    }
    pushToast({
      title: `Active: ${p.name}`,
      body: boot?.is_resume
        ? `Resumed session · ${boot.context_files_loaded} context file(s) loaded.`
        : `Project context bootstrapped · ${boot?.context_files_loaded ?? 0} file(s) loaded.`,
      kind: "success",
    });
  } catch (err) {
    console.warn("bootstrapProjectSession failed", err);
    pushToast({
      title: `Active: ${p.name}`,
      body: "Project switched (context bootstrap skipped).",
      kind: "info",
    });
  }
}

/** Open a project by filesystem path: refresh the project list into the
 *  store, find the matching code-project row (the backend registers cloned
 *  repos under their canonical path), activate it, and reveal the Projects
 *  sidebar. Returns false when no matching project is discoverable. */
export async function openProjectByPath(path: string): Promise<boolean> {
  const projects = await listProjects();
  useCortexStore.getState().setProjects(projects);
  const p = projects.find((x) => x.kind === "code" && x.root === path);
  if (!p) return false;
  await openCodeProject(p);
  useCortexStore.getState().setActivityTab("projects");
  return true;
}
