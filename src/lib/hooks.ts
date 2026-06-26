/**
 * Thin TS wrapper around the read-only `list_hooks` Tauri command.
 *
 * The backend owns the `.cortex/hooks/hooks.json` config; this side just reads
 * it back as a typed struct. Note the returned struct fields are snake_case
 * (`timeout_ms`) while the command arg key is camelCase (`projectRoot`).
 */

/** A single configured hook: a command + args, with an optional timeout. */
export interface HookSpec {
  command: string;
  args: string[];
  timeout_ms: number | null;
}

/** The project's hooks config, keyed by event name. */
export interface HooksConfig {
  events: Record<string, HookSpec[]>;
}

/** Read the configured hooks for a project (read-only). */
export async function listHooks(projectRoot: string): Promise<HooksConfig> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<HooksConfig>("list_hooks", { projectRoot });
}
