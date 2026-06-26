import { invoke } from "@tauri-apps/api/core";

/**
 * Tauri-bridge wrapper around the `config_files` backend. Mirrors the Rust
 * `ConfigTarget` / `ConfigReadResult` shapes so the schema editor can render
 * results without any post-processing.
 *
 * Each `ConfigPreset` is one entry in the modal's dropdown — `id` is the
 * stable key, `label` is the user-facing name, `target` is the backend
 * descriptor, and `hint` is the static schema-shape string shown in the
 * right-hand panel. Keep these in lockstep with the Rust validators.
 */

export type ConfigScope = "home" | "project";

export interface ConfigTarget {
  scope: ConfigScope;
  rel_path: string;
}

export interface ConfigReadResult {
  path: string;
  body: string;
  exists: boolean;
  read_only: boolean;
}

export interface ConfigPreset {
  id: string;
  label: string;
  target: ConfigTarget;
  /** Best-effort static description of the expected JSON shape. */
  hint: string;
  /** Display path shown next to the dropdown entry. */
  display: string;
  /** TOML/other non-editable preview (read-only flag also enforced server-side). */
  readOnly?: boolean;
}

/**
 * Curated set of Cortex config files. Order matters — the first entry is
 * the default selection when the modal opens with no `name` arg.
 */
export const CONFIG_PRESETS: ConfigPreset[] = [
  {
    id: "snippets",
    label: "Snippets",
    target: { scope: "home", rel_path: "snippets.json" },
    display: "~/.cortex/snippets.json",
    hint: `{
  "<name>": {
    "body": "string — the snippet text",
    "created_unix_ms": 0,
    "last_used_unix_ms": 0
  }
}`,
  },
  {
    id: "agent-instructions",
    label: "Agent Instructions",
    target: { scope: "home", rel_path: "agent-instructions.json" },
    display: "~/.cortex/agent-instructions.json",
    hint: `{
  "<agent_id>": "string — per-agent system-prompt suffix"
}`,
  },
  {
    id: "auto-approve",
    label: "Auto-Approve Rules",
    target: { scope: "home", rel_path: "auto-approve.json" },
    display: "~/.cortex/auto-approve.json",
    hint: `{
  "rules": [
    { "tool": "shell", "pattern": "^git status", "ttl_ms": 0 }
  ]
}`,
  },
  {
    id: "trust-matrix",
    label: "Trust Matrix",
    target: { scope: "home", rel_path: "trust-matrix.json" },
    display: "~/.cortex/trust-matrix.json",
    hint: `{
  "<project_root>": {
    "shell": true,
    "fs_write": false,
    "fs_read": true,
    "network": false
  }
}`,
  },
  {
    id: "webhooks",
    label: "Webhooks",
    target: { scope: "home", rel_path: "webhooks.json" },
    display: "~/.cortex/webhooks.json",
    hint: `{
  "webhooks": [
    {
      "id": "string",
      "url": "https://…",
      "events": ["task.complete"],
      "enabled": true
    }
  ]
}`,
  },
  {
    id: "themes",
    label: "Themes (custom)",
    target: { scope: "home", rel_path: "themes.json" },
    display: "~/.cortex/themes.json",
    hint: `{
  "<theme_name>": {
    "bg": "#…",
    "fg": "#…",
    "accent": "#…"
  }
}`,
  },
  {
    id: "hooks",
    label: "Project Hooks",
    target: { scope: "project", rel_path: "hooks.json" },
    display: ".cortex/hooks.json",
    hint: `{
  "pre_task":  [{ "command": "..." }],
  "post_task": [{ "command": "..." }]
}`,
  },
  {
    id: "danger",
    label: "Danger Profile (TOML)",
    target: { scope: "project", rel_path: "danger.toml" },
    display: ".cortex/danger.toml",
    hint: `# TOML — validated on save
[shell]
deny = ["rm -rf /"]`,
  },
];

export function findPreset(id: string): ConfigPreset | undefined {
  return CONFIG_PRESETS.find((p) => p.id === id);
}

export async function readConfigFile(
  target: ConfigTarget,
  projectRoot?: string | null,
): Promise<ConfigReadResult> {
  return invoke<ConfigReadResult>("read_config_file", {
    target,
    projectRoot: projectRoot ?? null,
  });
}

export async function writeConfigFile(
  target: ConfigTarget,
  body: string,
  projectRoot?: string | null,
): Promise<string> {
  return invoke<string>("write_config_file", {
    target,
    body,
    projectRoot: projectRoot ?? null,
  });
}
