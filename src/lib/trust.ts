/**
 * Thin TS wrappers around the project-trust Tauri commands.
 *
 * Cortex sandboxes untrusted projects to a read-only tier. The backend owns
 * the trust ledger; this side just forwards a project root and surfaces the
 * boolean state. Mirrors the `editor-save.ts` wrapper style — no UI, no
 * caching, the backend is the source of truth.
 *
 * Tauri maps the camelCase `projectRoot` JS key to the Rust `project_root`
 * parameter automatically, so callers pass `projectRoot` here.
 */
import { invoke } from "@tauri-apps/api/core";

/** Trust state for a single project root. `true` means the project is
 *  trusted (full tooling); `false` means it's sandboxed read-only. */
export type TrustStatus = boolean;

/**
 * Returns `true` when `projectRoot` is trusted, `false` when it's still
 * sandboxed to the read-only tier. Throws if the backend rejects the query —
 * callers should fail closed (treat as untrusted / render nothing) and toast.
 */
export async function getTrustStatus(projectRoot: string): Promise<TrustStatus> {
  return await invoke<TrustStatus>("get_trust_status", { projectRoot });
}

/**
 * Promote `projectRoot` to the trusted tier (full tooling). Throws if the
 * backend rejects the request; callers should surface the error to the user.
 */
export async function trustProject(projectRoot: string): Promise<void> {
  await invoke<void>("trust_project", { projectRoot });
}

/**
 * Demote `projectRoot` back to the sandboxed read-only tier. Throws on
 * backend rejection. (Not used by the banner — it's already untrusted there —
 * but provided for symmetry / other call sites.)
 */
export async function untrustProject(projectRoot: string): Promise<void> {
  await invoke<void>("untrust_project", { projectRoot });
}
