import { stopRun } from "@/lib/cortex-bridge";
import { humanizeError } from "@/lib/errors";
import { exportConversation } from "@/lib/conversation-export";
import { openMcpPanel } from "@/components/McpServersPanel";
import { openHooksPanel } from "@/components/HooksPanel";
import { listCheckpoints } from "@/lib/checkpoints";
import { reviewCheckpointRestore } from "@/lib/checkpoint-review";
import { KEEP_RECENT, shouldCompact } from "@/lib/compressor";
import { performCondense } from "@/lib/condense";
import { estimateContextBreakdown, fetchUrl, type ContextBreakdown } from "@/lib/context";
import { confirmDialog } from "@/lib/dialogs";
import { desktopNotify } from "@/lib/notify";
import { createPrp } from "@/lib/prp";
import { repoMapText } from "@/lib/repo-map";
import { formatShellResult, shellExec } from "@/lib/shell-run";
import { pushToast } from "@/lib/toast";
import { useCortexStore, type ActivityTab, type Message } from "@/state/store";

/**
 * Slash command registry. Centralises every `/cmd` the chat input understands
 * so ChatPane stays small and ShortcutsModal stays accurate. To add a new
 * command, append an entry to COMMANDS — no other file needs to change.
 */

type StoreApi = {
  getState: typeof useCortexStore.getState;
  setState: typeof useCortexStore.setState;
};

export interface SlashContext {
  store: StoreApi;
  append: (msg: Message) => void;
  notify: (title: string, body?: string, kind?: "info" | "success" | "error" | "warning") => void;
}

export interface SlashCommand {
  /** Canonical name without the leading slash. */
  name: string;
  /** Optional alternate names matched alongside `name`. */
  aliases?: string[];
  /** One-liner shown in autocomplete and the shortcuts cheat sheet. */
  description: string;
  /** Optional usage hint (e.g. "<text>") rendered after the name. */
  usage?: string;
  /** Implementation. `args` is the raw string after the first whitespace. */
  run: (args: string, ctx: SlashContext) => void | Promise<void>;
  /** Optional UI grouping bucket. Resolved at render-time via `categorize()`
   *  when omitted — kept as a field so future entries can override the
   *  name-pattern fallback without touching the helper. */
  category?: string;
}

function systemNote(content: string): Message {
  return { id: `s-${crypto.randomUUID()}`, role: "system", content, tools: [] };
}
function errorNote(content: string): Message {
  return { id: `e-${crypto.randomUUID()}`, role: "error", content, tools: [] };
}

/** Outcome of the backend `run_test_command` (aider `--test-cmd`). */
type TestRunOutcome = {
  command: string;
  exitCode: number | null;
  passed: boolean;
  stdout: string;
  stderr: string;
  durationMs: number;
  truncated: boolean;
  timedOut: boolean;
};

/** Render a test-run outcome as a chat system note (pass/fail headline + a
 *  fenced output tail when there's anything to show). */
function formatTestOutcome(o: TestRunOutcome): string {
  const head = o.timedOut
    ? `⏱️ Tests timed out — \`${o.command}\` (${o.durationMs} ms)`
    : o.passed
      ? `✅ Tests passed — \`${o.command}\` (${o.durationMs} ms)`
      : `❌ Tests failed — \`${o.command}\` (exit ${o.exitCode ?? "killed"}, ${o.durationMs} ms)`;
  const body = [o.stdout, o.stderr].filter((s) => s.trim()).join("\n").trim();
  const trunc = o.truncated ? "\n\n_(output tail shown — earlier lines clipped)_" : "";
  return body ? `${head}\n\n\`\`\`\n${body}\n\`\`\`${trunc}` : head;
}

/** Run the project's configured test command, append the result to the chat,
 *  and toast pass/fail. Returns the outcome, or null when nothing ran. With
 *  `silentIfUnset`, an unconfigured project is a quiet no-op (used by the
 *  `/apply` auto-test hook so it never nags projects that don't opt in). */
async function runConfiguredTests(
  ctx: SlashContext,
  root: string,
  opts: { silentIfUnset?: boolean } = {},
): Promise<TestRunOutcome | null> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const outcome = await invoke<TestRunOutcome>("run_test_command", { projectRoot: root });
    ctx.append(systemNote(formatTestOutcome(outcome)));
    ctx.notify(
      outcome.passed ? "Tests passed" : outcome.timedOut ? "Tests timed out" : "Tests failed",
      outcome.command,
      outcome.passed ? "success" : "error",
    );
    return outcome;
  } catch (e) {
    const msg = humanizeError(e);
    const unset = msg.includes("no test command configured");
    if (opts.silentIfUnset && unset) return null;
    ctx.notify("Run tests failed", msg, unset ? "warning" : "error");
    return null;
  }
}

/** Outcome of the backend `run_lint` (aider `--lint-cmd`). */
type LintRunOutcome = {
  command: string;
  fromOverride: boolean;
  exitCode: number | null;
  clean: boolean;
  stdout: string;
  stderr: string;
  durationMs: number;
  truncated: boolean;
  timedOut: boolean;
};

/** Render a lint-run outcome as a chat system note. Unlike tests, a clean exit
 *  code doesn't always mean "no findings" (plain `cargo clippy` exits 0 with
 *  warnings), so we always show captured output and keep the headline factual. */
function formatLintOutcome(o: LintRunOutcome): string {
  const body = [o.stdout, o.stderr].filter((s) => s.trim()).join("\n").trim();
  const head = o.timedOut
    ? `⏱️ Lint timed out — \`${o.command}\` (${o.durationMs} ms)`
    : o.clean
      ? body
        ? `🔍 Lint clean (exit 0) — \`${o.command}\` (${o.durationMs} ms)`
        : `✅ Lint clean — \`${o.command}\` (${o.durationMs} ms)`
      : `⚠️ Lint found issues — \`${o.command}\` (exit ${o.exitCode ?? "killed"}, ${o.durationMs} ms)`;
  const trunc = o.truncated ? "\n\n_(output head shown — later lines clipped)_" : "";
  return body ? `${head}\n\n\`\`\`\n${body}\n\`\`\`${trunc}` : head;
}

/** One file in the aider-style chat manifest (`/add`). Mirrors the backend
 *  `ManifestEntry` — a project-relative path plus its live on-disk state. */
type ManifestEntry = { path: string; exists: boolean; size: number | null };

/** Result of an `/add` (mirrors the backend `AddResult`). */
type ManifestAddResult = {
  added: string[];
  already: string[];
  skipped: { path: string; reason: string }[];
  manifest: ManifestEntry[];
};

/** Render the manifest as a chat system note (used by `/ls` and after edits). */
function formatManifest(entries: ManifestEntry[]): string {
  if (entries.length === 0) {
    return "📂 No files in the chat. Add some with `/add <path>` so their contents stay in context.";
  }
  const lines = entries.map((e) => {
    const flag = e.exists ? "" : "  _(missing on disk)_";
    const kb = e.exists && e.size != null ? `  (${(e.size / 1024).toFixed(1)} KB)` : "";
    return `- \`${e.path}\`${kb}${flag}`;
  });
  return `📂 Files in the chat (${entries.length}) — their full contents are sent with every message:\n${lines.join("\n")}\n\nDrop one with \`/drop <path>\`, or clear all with \`/drop\`.`;
}

/** One knowledge microagent definition (mirrors the backend `MicroAgentInfo`). */
type MicroAgentInfo = { name: string; triggers: string[]; bytes: number };

/** Render the project's knowledge microagents as a chat system note (`/microagents`). */
function formatMicroAgents(agents: MicroAgentInfo[]): string {
  if (agents.length === 0) {
    return "🧠 No knowledge microagents in this project. Add a `*.md` file under `.cortex/microagents/` with `triggers:` frontmatter — its body is injected into context whenever a trigger word appears in your message.";
  }
  const lines = agents.map((a) => {
    const trig = a.triggers.map((t) => `\`${t}\``).join(", ");
    const kb = (a.bytes / 1024).toFixed(1);
    return `- **${a.name}** — triggers: ${trig}  _(${kb} KB)_`;
  });
  return `🧠 Knowledge microagents (${agents.length}) — injected automatically when a trigger word appears in a message:\n${lines.join("\n")}`;
}

/** Split a raw slash arg into file paths, honoring double-quotes so a path with
 *  spaces (`/add "my file.ts"`) stays one token. */
function splitPaths(raw: string): string[] {
  const out: string[] = [];
  const re = /"([^"]+)"|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(raw)) !== null) out.push(m[1] ?? m[2]);
  return out;
}

/** Factory: a command that just switches the activity sidebar. */
function tabCmd(
  name: string,
  tab: NonNullable<ActivityTab>,
  description: string,
  aliases?: string[],
): SlashCommand {
  return { name, aliases, description, run: (_a, ctx) => ctx.store.getState().setActivityTab(tab) };
}

export const COMMANDS: SlashCommand[] = [
  {
    name: "clear",
    aliases: ["new", "reset"],
    description: "Start a fresh session (no AI call)",
    run: (_a, ctx) => ctx.store.getState().resetSession(),
  },
  {
    name: "resume",
    description: "Open the session resume picker",
    // Store-driven (same flag the Ctrl+R shortcut and <SessionPicker/> use).
    run: (_a, ctx) => ctx.store.getState().setShowSessionPicker(true),
  },
  {
    name: "help",
    description: "Open the Help panel (or use Ctrl+K → 'shortcuts')",
    run: (_a, ctx) => {
      // Wave 146 — actually open the Help activity tab instead of just
      // telling the user to find it in the command palette.
      ctx.store.getState().setActivityTab("help");
    },
  },
  {
    // `/shortcuts` pops the keyboard cheat sheet as a self-mounting portal
    // (same pattern as /changelog → openChangelogModal), rather than jumping
    // to the Help activity tab. App.tsx also binds Ctrl+? to the same modal.
    name: "shortcuts",
    description: "Open the keyboard-shortcuts cheat sheet",
    run: async (_a, ctx) => {
      try {
        const { openShortcutsModal } = await import("@/components/ShortcutsModal");
        openShortcutsModal();
      } catch (e) {
        ctx.append(errorNote(`/shortcuts failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    name: "settings",
    description: "Open the settings modal",
    run: (_a, ctx) => ctx.store.getState().setShowSettings(true),
  },
  {
    name: "tokens",
    description: "Show an Aider-style breakdown of what's in the context window",
    run: async (_a, ctx) => {
      const state = ctx.store.getState();
      const sessionId = state.sessionId;
      const projectRoot = state.activeProject?.root;
      try {
        const backend = await estimateContextBreakdown(sessionId, projectRoot);
        // The backend gives us system + claude_md + rules; history and
        // attached files we compute locally from the in-memory store so
        // we don't depend on session persistence being wired.
        const local = localContextStats(state.messages);
        const merged: ContextBreakdown = {
          system_chars: backend.system_chars,
          claude_md_chars: backend.claude_md_chars,
          rules_chars: backend.rules_chars,
          repo_map_chars: backend.repo_map_chars,
          history_chars: backend.history_chars || local.history_chars,
          history_message_count:
            backend.history_message_count || local.history_message_count,
          attached_files_chars:
            backend.attached_files_chars || local.attached_files_chars,
          total_estimated_tokens: 0,
        };
        const totalChars =
          merged.system_chars +
          merged.claude_md_chars +
          merged.rules_chars +
          merged.repo_map_chars +
          merged.history_chars +
          merged.attached_files_chars;
        merged.total_estimated_tokens = Math.floor(totalChars / 4);
        ctx.append(tokensBreakdownMessage(merged));
      } catch (e) {
        ctx.append(errorNote(`/tokens failed: ${humanizeError(e)}`));
      }
    },
  },
  tabCmd("usage", "usage", "Jump to the Usage panel"),
  tabCmd("brain", "brain", "Jump to the Brain panel"),
  tabCmd("memory", "memory", "Jump to the Memory panel"),
  tabCmd("projects", "projects", "Jump to the Projects panel"),
  tabCmd("obs", "observability", "Jump to the Observability panel", ["observability"]),
  // P0-FINAL Wave 5 — the five newest tabs were unreachable from the palette
  // (it derives its entries from this registry) and from slash commands.
  // `/research` already exists above as a full command (it can also start a
  // run); these four just jump. Together they make all five keyboard-reachable.
  tabCmd("cookbook", "cookbook", "Jump to the model Cookbook (pull + serve local models)"),
  tabCmd("routines", "routines", "Jump to the Routines scheduler", ["routine"]),
  tabCmd("eval", "eval", "Jump to the Eval harness (run + compare reports)", ["evals"]),
  tabCmd("setup", "setup", "Jump to the Setup panel (git server + onboarding)"),
  {
    name: "worktree",
    aliases: ["wt"],
    description: "Open the worktree picker",
    // The picker is mounted (prop-driven) in ProjectSidebar; it listens for
    // this window event so the chat command can pop it open from anywhere.
    run: (_a, _ctx) => window.dispatchEvent(new Event("cortex:open-worktrees")),
  },
  {
    name: "stop",
    description: "Stop all currently running runs",
    run: async (_a, ctx) => {
      const ids = ctx.store.getState().runningRunIds;
      if (ids.length === 0) {
        ctx.notify("Nothing to stop", "No active runs.", "info");
        return;
      }
      for (const rid of ids) {
        try { await stopRun(rid); } catch { /* best-effort */ }
      }
    },
  },
  {
    name: "compact",
    aliases: ["condense"],
    description: "Condense older turns into an LLM summary, keep last 8 verbatim",
    run: async (_a, ctx) => {
      const state = ctx.store.getState();
      if (!shouldCompact(state.messages.length, KEEP_RECENT)) {
        ctx.notify("Compact skipped", `Only ${state.messages.length} messages — nothing to fold.`, "info");
        return;
      }
      // The single shared condenser (also used by the TokenHUD button and
      // auto-condense-on-overflow): real LLM summary, heuristic fallback,
      // adopt onto the active thread.
      await performCondense({
        model: state.selectedModel,
        keepRecent: KEEP_RECENT,
        notify: ctx.notify,
      });
    },
  },
  {
    name: "note",
    description: "Add a system-level note to this chat",
    usage: "<text>",
    run: (args, ctx) => {
      const text = args.trim();
      if (!text) {
        ctx.notify("Note skipped", "Usage: /note <text>", "warning");
        return;
      }
      ctx.append(systemNote(`📝 ${text}`));
    },
  },
  {
    name: "export",
    aliases: ["save"],
    description: "Export conversation — '/export' to clipboard, '/export md|json' to a file under the project root",
    run: async (args, ctx) => {
      const state = ctx.store.getState();
      const messages = state.messages;
      if (messages.length === 0) {
        ctx.notify("Export skipped", "No messages to export.", "info");
        return;
      }
      const mode = args.trim().toLowerCase();
      if (mode === "md" || mode === "json") {
        try {
          const path = await exportConversation(mode, messages, {
            sessionId: state.sessionId,
            project: state.activeProject?.root,
          });
          ctx.notify(`Exported (.${mode})`, path, "success");
        } catch (e) {
          ctx.notify("Export failed", humanizeError(e), "error");
        }
        return;
      }
      // Default / unrecognized arg → clipboard markdown (back-compat).
      const md = messages.map((m) => `### ${m.agent ?? m.role}\n\n${m.content}\n`).join("\n");
      try {
        await navigator.clipboard.writeText(md);
        ctx.notify("Copied", `${messages.length} messages copied as markdown.`, "success");
      } catch (e) {
        ctx.notify("Copy failed", humanizeError(e), "error");
      }
    },
  },
  {
    name: "mcp",
    aliases: ["servers"],
    description: "Manage MCP servers — connect to local Model Context Protocol tools",
    run: (_a, _ctx) => openMcpPanel(),
  },
  {
    name: "hooks",
    aliases: ["hook"],
    description: "Inspect the project's configured hooks (read-only)",
    run: (_a, _ctx) => openHooksPanel(),
  },
  {
    name: "newwindow",
    aliases: ["window"],
    description: "Open a second Cortex window",
    run: async (_a, ctx) => {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke("open_secondary_window");
        ctx.notify("New window", "Opened a second Cortex window.", "success");
      } catch (e) {
        ctx.notify("New window failed", humanizeError(e), "error");
      }
    },
  },
  {
    name: "import-mem",
    aliases: ["importmem", "claudemem"],
    description: "Import Claude Code project memory into the Cortex brain",
    run: async (_a, ctx) => {
      ctx.notify("Importing…", "Scanning Claude project memory.", "info");
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const r = await invoke<{
          scanned: number;
          imported: number;
          skipped: number;
          destination: string;
        }>("import_claude_mem");
        ctx.notify(
          "Memory imported",
          `${r.imported} imported, ${r.skipped} skipped of ${r.scanned} → ${r.destination}`,
          "success",
        );
      } catch (e) {
        ctx.notify("Import failed", humanizeError(e), "error");
      }
    },
  },
  {
    name: "retrieve",
    aliases: ["context", "ctx"],
    description: "Find the most relevant project context for a query and insert it into the composer",
    run: async (args, ctx) => {
      const query = args.trim();
      if (!query) {
        ctx.notify("Retrieve", "Usage: /retrieve <query>", "info");
        return;
      }
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Retrieve", "Open a project first.", "warning");
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const hits = await invoke<
          { source: string; path: string; snippet: string; score: number }[]
        >("retrieve", { projectRoot: root, query, k: 6 });
        if (!hits.length) {
          ctx.notify("Retrieve", "No relevant context found.", "info");
          return;
        }
        const block = hits
          .map((h) => `- [${h.source}] ${h.path}${h.snippet ? ` — ${h.snippet}` : ""}`)
          .join("\n");
        window.dispatchEvent(
          new CustomEvent("cortex:composer-insert", {
            detail: { value: `Relevant context for "${query}":\n${block}\n` },
          }),
        );
        ctx.notify("Retrieve", `Inserted ${hits.length} context hits.`, "success");
      } catch (e) {
        ctx.notify("Retrieve failed", humanizeError(e), "error");
      }
    },
  },
  {
    name: "rerank",
    aliases: ["rr"],
    description: "Retrieve project context and LLM-rerank it by relevance, then insert into the composer",
    run: async (args, ctx) => {
      const query = args.trim();
      if (!query) {
        ctx.notify("Rerank", "Usage: /rerank <query>", "info");
        return;
      }
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Rerank", "Open a project first.", "warning");
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const hits = await invoke<
          { source: string; path: string; snippet: string; score: number }[]
        >("rerank", { projectRoot: root, query, k: 6 });
        if (!hits.length) {
          ctx.notify("Rerank", "No relevant context found.", "info");
          return;
        }
        const block = hits
          .map((h) => `- [${h.source}] ${h.path}${h.snippet ? ` — ${h.snippet}` : ""}`)
          .join("\n");
        window.dispatchEvent(
          new CustomEvent("cortex:composer-insert", {
            detail: { value: `Reranked context for "${query}":\n${block}\n` },
          }),
        );
        ctx.notify("Rerank", `Inserted ${hits.length} reranked hits.`, "success");
      } catch (e) {
        ctx.notify("Rerank failed", humanizeError(e), "error");
      }
    },
  },
  {
    name: "voice",
    aliases: ["mic"],
    description: "Start voice input (browser SpeechRecognition, whisper.cpp fallback)",
    run: async (_a, ctx) => {
      // Feature-detected so unsupported browsers get a graceful toast
      // instead of an unhandled ReferenceError.
      const w = window as unknown as {
        SpeechRecognition?: new () => SpeechRecognitionLike;
        webkitSpeechRecognition?: new () => SpeechRecognitionLike;
      };
      const Ctor = w.SpeechRecognition ?? w.webkitSpeechRecognition;
      if (!Ctor) {
        // Browser SpeechRecognition is missing (common on Linux/Firefox/the
        // Tauri webview). Fall back to capturing a short clip and shipping it
        // through whisper.cpp via the Rust `voice_transcribe` command.
        try {
          const { recordAndTranscribe } = await import("@/lib/voice-fallback");
          ctx.notify("Listening… (whisper)", "Recording a short clip — speak now.", "info");
          const { promise, stop } = recordAndTranscribe();
          // The toast system has no action-button slot, so we surface the
          // early-stop affordance via the in-app confirm dialog. "Stop now"
          // ends the clip; "Keep recording" lets the 4s safety timeout finish
          // it. The recorder keeps buffering on its own thread while the
          // dialog is up, so audio isn't lost.
          if (
            await confirmDialog({
              title: "Recording…",
              message: "Stop the clip now, or keep recording until the 4s auto-stop.",
              confirmLabel: "Stop now",
              cancelLabel: "Keep recording",
            })
          ) {
            stop();
          }
          const text = (await promise).trim();
          if (text) ctx.append(systemNote(`🎤 ${text}`));
          else ctx.notify("Voice", "No speech captured.", "info");
        } catch (e) {
          ctx.notify("Voice unavailable", `whisper fallback failed: ${humanizeError(e)}`, "warning");
        }
        return;
      }
      try {
        const rec = new Ctor();
        rec.lang = navigator.language || "en-US";
        rec.interimResults = false;
        rec.onresult = (ev) => {
          const text = ev.results[0]?.[0]?.transcript ?? "";
          if (text) ctx.append(systemNote(`🎤 ${text}`));
        };
        rec.onerror = (ev) => ctx.notify("Voice error", ev.error ?? "unknown", "error");
        rec.start();
        ctx.notify("Listening…", "Speak now.", "info");
      } catch (e) {
        ctx.notify("Voice failed", humanizeError(e), "error");
      }
    },
  },
  {
    name: "repomap",
    description: "Inject the active project's repo map as a system message",
    run: async (_a, ctx) => {
      const project = ctx.store.getState().activeProject;
      if (!project) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      try {
        const text = await repoMapText(project.root);
        ctx.append(systemNote(`🗺️ Repo map for ${project.name}\n\n${text}`));
      } catch (e) {
        ctx.append(errorNote(`repo map failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    name: "web",
    aliases: ["url", "fetch"],
    description: "Fetch a URL and inject its content as markdown (Aider-style)",
    usage: "<url>",
    run: async (args, ctx) => {
      const url = args.trim();
      if (!url) {
        ctx.notify("/web skipped", "Usage: /web <url>", "warning");
        return;
      }
      ctx.append(systemNote(`🌐 fetching ${url}…`));
      try {
        const page = await fetchUrl(url);
        const head = page.title
          ? `🌐 **${page.title}** — ${page.url}`
          : `🌐 ${page.url}`;
        const trunc = page.truncated ? "\n\n_…response truncated at 256 KiB._" : "";
        ctx.append(systemNote(`${head}\n\n${page.markdown}${trunc}`));
      } catch (e) {
        ctx.append(errorNote(`/web failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    name: "focus",
    aliases: ["chain", "todo"],
    description: "Show / jump to the agent's focus chain (live to-do list)",
    run: (_a, ctx) => ctx.store.getState().setActivityTab("focus"),
  },
  {
    name: "trust",
    description: "Open the Trust Matrix (granular auto-approve toggles)",
    run: (_a, ctx) => ctx.store.getState().setActivityTab("trust"),
  },
  {
    name: "run",
    aliases: ["sh", "exec"],
    description: "Run a shell command in the active project and stream the output back",
    usage: "<command>",
    run: async (args, ctx) => {
      const cmd = args.trim();
      if (!cmd) {
        ctx.notify("/run skipped", "Usage: /run <command>", "warning");
        return;
      }
      const projectRoot = ctx.store.getState().activeProject?.root;
      ctx.append(systemNote(`\`$ ${cmd}\` — running…`));
      try {
        const result = await shellExec(cmd, projectRoot ?? null);
        ctx.append(systemNote(formatShellResult(cmd, result)));
      } catch (e) {
        ctx.append(errorNote(`/run failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Aider-style two-phase plan→edit (`/architect on|off|status`).
    //
    // The Zustand flag is the single source of truth; cortex-bridge.ts
    // reads it back from localStorage when assembling chat_send so we
    // don't need to touch the chat call sites. `planner_model=<m>` /
    // `editor_model=<m>` slot in optional overrides.
    name: "architect",
    description: "Toggle Aider-style planner/editor dual-model mode",
    usage: "on|off|status|planner_model=<m>|editor_model=<m>",
    run: (args, ctx) => {
      const raw = args.trim();
      const s = ctx.store.getState();
      if (!raw || raw === "status") {
        const planner = s.plannerModel ?? "(default)";
        const editor = s.editorModel ?? "(default)";
        ctx.append(
          systemNote(
            `🏛️ architect mode: **${s.architectMode ? "on" : "off"}**\n- planner: \`${planner}\`\n- editor: \`${editor}\``,
          ),
        );
        return;
      }
      if (raw === "on" || raw === "off") {
        s.setArchitectMode(raw === "on");
        ctx.notify("Architect mode", `${raw === "on" ? "enabled" : "disabled"}.`, "success");
        return;
      }
      // Model overrides: `planner_model=foo` / `editor_model=bar`. Multiple
      // can be space-separated. Unknown keys fall through to the usage hint.
      let planner = s.plannerModel;
      let editor = s.editorModel;
      let matched = false;
      for (const part of raw.split(/\s+/)) {
        const m = part.match(/^(planner_model|editor_model)=(.+)$/);
        if (!m) continue;
        matched = true;
        const value = m[2] === "default" || m[2] === "" ? null : m[2];
        if (m[1] === "planner_model") planner = value;
        else editor = value;
      }
      if (!matched) {
        ctx.notify("/architect", "Usage: /architect on|off|status|planner_model=<m>|editor_model=<m>", "warning");
        return;
      }
      s.setArchitectModels(planner, editor);
      ctx.notify("Architect models updated", `planner=${planner ?? "default"} editor=${editor ?? "default"}`, "success");
    },
  },
  {
    // Aider-style SEARCH/REPLACE block applier. Tool-less models (a local
    // Ollama, say) can only *describe* edits in their reply; this parses the
    // last assistant message for `<<<<<<< SEARCH … >>>>>>> REPLACE` blocks and
    // applies them to files under the open project. `/apply preview` matches
    // without writing so you can check before committing.
    name: "apply",
    aliases: ["apply-edits"],
    description: "Apply SEARCH/REPLACE edit blocks from the last reply to project files",
    usage: "[preview]",
    run: async (args, ctx) => {
      const dryRun = /^(preview|dry|dry-run|check)$/i.test(args.trim());
      const state = ctx.store.getState();
      const root = state.activeProject?.root;
      if (!root) {
        ctx.notify("Apply edits", "Open a project first.", "warning");
        return;
      }
      let text: string | undefined;
      for (let i = state.messages.length - 1; i >= 0; i--) {
        if (state.messages[i].role === "assistant" && state.messages[i].content.trim()) {
          text = state.messages[i].content;
          break;
        }
      }
      if (!text) {
        ctx.notify("Apply edits", "No assistant reply to apply.", "info");
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const report = await invoke<{
          results: { path: string; status: string; reason?: string; searchLines: number; replaceLines: number }[];
          applied: number;
          created: number;
          failed: number;
          dryRun: boolean;
          checkpointId?: string;
        }>("apply_edit_blocks", { projectRoot: root, text, dryRun });
        if (!report.results.length) {
          ctx.notify("Apply edits", "No SEARCH/REPLACE blocks found in the reply.", "info");
          return;
        }
        const icon = (s: string) =>
          s === "applied" ? "✓" : s === "created" ? "+" : "✗";
        const lines = report.results
          .map((r) => `- ${icon(r.status)} \`${r.path}\` — ${r.status}${r.reason ? ` (${r.reason})` : ""}`)
          .join("\n");
        const verb = report.dryRun ? "Would apply" : "Applied";
        const summary = `${verb}: ${report.applied} edited · ${report.created} created · ${report.failed} failed`;
        // A real apply that changed files snapshots the workspace first (Cline-style),
        // so the user can roll back from the Checkpoints panel.
        const undoNote = report.checkpointId
          ? `\n\n↩ Snapshot saved before applying — open the **Checkpoints** panel to restore (undo).`
          : "";
        ctx.append(systemNote(`**${summary}**\n${lines}${undoNote}`));
        ctx.notify(
          report.dryRun ? "Edit preview" : "Edits applied",
          summary,
          report.failed > 0 ? "warning" : "success",
        );
        // aider `--auto-test`: after a real apply that actually changed files,
        // run the project's configured test command (if any) so the edit is
        // *verified*, not assumed. Quiet when no command is set.
        if (!report.dryRun && report.applied + report.created > 0) {
          await runConfiguredTests(ctx, root, { silentIfUnset: true });
        }
      } catch (e) {
        ctx.notify("Apply edits failed", humanizeError(e), "error");
      }
    },
  },
  {
    // Aider's `--test-cmd`: configure the shell command that runs this project's
    // test suite, persisted at `.cortex/test-command.toml`. Distinct from
    // `/test` (which auto-detects a framework and opens a panel) — this is an
    // explicit, persisted command that also auto-runs after `/apply`. No args
    // shows the current setting; `/testcmd clear` unsets it.
    name: "testcmd",
    aliases: ["test-cmd"],
    description: "Set/show the project's test command (auto-runs after /apply)",
    usage: "[command | clear]",
    run: async (args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Test command", "Open a project first.", "warning");
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const raw = args.trim();
        if (!raw) {
          const cur = await invoke<string | null>("get_test_command", { projectRoot: root });
          ctx.append(
            systemNote(
              cur
                ? `🧪 Test command: \`${cur}\`\n\nRun it now with \`/retest\`. It also runs automatically after a successful \`/apply\`.`
                : "🧪 No test command set. Set one with `/testcmd <command>` (e.g. `/testcmd cargo test`). It will then run automatically after `/apply`.",
            ),
          );
          return;
        }
        const next = /^(clear|unset|none|off)$/i.test(raw) ? "" : raw;
        await invoke("set_test_command", { projectRoot: root, command: next });
        ctx.notify("Test command", next ? `Set to: ${next}` : "Cleared.", "success");
      } catch (e) {
        ctx.notify("Test command failed", humanizeError(e), "error");
      }
    },
  },
  {
    // Run the configured test command now (aider's manual test run). Reports the
    // pass/fail headline + output tail inline; prompts `/testcmd` when unset.
    name: "retest",
    aliases: ["test-run", "autotest"],
    description: "Run the configured test command now and report pass/fail",
    run: async (_args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Run tests", "Open a project first.", "warning");
        return;
      }
      ctx.append(systemNote("🧪 Running tests…"));
      await runConfiguredTests(ctx, root);
    },
  },
  {
    // Aider's `/lint`: run the project's linter and surface violations. The
    // command is auto-detected from the project (its own `npm run lint`,
    // ESLint/Ruff config, `cargo clippy`, `go vet`) or taken from a persisted
    // override set with `/lintcmd`. Manual run — prints the head of the output.
    name: "lint",
    description: "Run the project's linter (auto-detected) and surface violations",
    run: async (_args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Lint", "Open a project first.", "warning");
        return;
      }
      ctx.append(systemNote("🔍 Linting…"));
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const outcome = await invoke<LintRunOutcome>("run_lint", { projectRoot: root });
        ctx.append(systemNote(formatLintOutcome(outcome)));
        ctx.notify(
          outcome.clean ? "Lint clean" : outcome.timedOut ? "Lint timed out" : "Lint found issues",
          outcome.command,
          outcome.clean ? "success" : "warning",
        );
      } catch (e) {
        const msg = humanizeError(e);
        const undetected = msg.includes("no linter detected");
        ctx.notify("Lint failed", msg, undetected ? "warning" : "error");
      }
    },
  },
  {
    // Set/show/clear the lint-command override (aider's `--lint-cmd`), persisted
    // at `.cortex/lint-command.toml`. No args shows the resolved command
    // (override or auto-detected); `/lintcmd clear` reverts to auto-detection.
    name: "lintcmd",
    aliases: ["lint-cmd"],
    description: "Set/show the project's lint command override",
    usage: "[command | clear]",
    run: async (args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Lint command", "Open a project first.", "warning");
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const raw = args.trim();
        if (!raw) {
          const override = await invoke<string | null>("get_lint_command", { projectRoot: root });
          const resolved = await invoke<string | null>("detect_lint", { projectRoot: root });
          ctx.append(
            systemNote(
              override
                ? `🔍 Lint command (override): \`${override}\`\n\nRun it with \`/lint\`. Clear with \`/lintcmd clear\` to use auto-detection.`
                : resolved
                  ? `🔍 No override set — auto-detected: \`${resolved}\`\n\nRun it with \`/lint\`, or pin a command with \`/lintcmd <command>\`.`
                  : "🔍 No linter detected for this project. Set one with `/lintcmd <command>` (e.g. `/lintcmd cargo clippy`).",
            ),
          );
          return;
        }
        const next = /^(clear|unset|none|off)$/i.test(raw) ? "" : raw;
        await invoke("set_lint_command", { projectRoot: root, command: next });
        ctx.notify("Lint command", next ? `Set to: ${next}` : "Cleared (auto-detect).", "success");
      } catch (e) {
        ctx.notify("Lint command failed", humanizeError(e), "error");
      }
    },
  },
  {
    // Aider's `/add`: put files explicitly *in the chat* so their full current
    // contents are sent to the model with every message (the user's deliberate
    // working set, persisted at `.cortex/manifest.json`). Paths are confined to
    // the open project; quote a path with spaces. Distinct from `@`-mentions
    // (one-shot) and the ranked repo-map (signatures only).
    name: "add",
    aliases: ["add-file"],
    description: "Add file(s) to the chat so their contents stay in context",
    usage: "<path> [path…]",
    run: async (args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Add to chat", "Open a project first.", "warning");
        return;
      }
      const paths = splitPaths(args.trim());
      if (paths.length === 0) {
        ctx.append(systemNote("Usage: `/add <path>` — e.g. `/add src/main.rs`. List with `/ls`."));
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const res = await invoke<ManifestAddResult>("add_to_manifest", {
          projectRoot: root,
          paths,
        });
        const parts: string[] = [];
        if (res.added.length) parts.push(`Added ${res.added.map((p) => `\`${p}\``).join(", ")}.`);
        if (res.already.length)
          parts.push(`Already in chat: ${res.already.map((p) => `\`${p}\``).join(", ")}.`);
        for (const s of res.skipped) parts.push(`Skipped \`${s.path}\`: ${s.reason}.`);
        parts.push("", formatManifest(res.manifest));
        ctx.append(systemNote(parts.join("\n")));
        if (res.added.length)
          ctx.notify("Added to chat", `${res.added.length} file(s) now in context`, "success");
        else if (res.skipped.length)
          ctx.notify("Add to chat", `${res.skipped.length} path(s) skipped`, "warning");
      } catch (e) {
        ctx.notify("Add to chat failed", humanizeError(e), "error");
      }
    },
  },
  {
    // Aider's `/drop`: remove file(s) from the chat manifest, or clear it
    // entirely with a bare `/drop`.
    name: "drop",
    aliases: ["drop-file"],
    description: "Remove file(s) from the chat (bare /drop clears all)",
    usage: "[path…]",
    run: async (args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Drop from chat", "Open a project first.", "warning");
        return;
      }
      const paths = splitPaths(args.trim());
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const manifest = await invoke<ManifestEntry[]>("drop_from_manifest", {
          projectRoot: root,
          paths,
        });
        const head = paths.length === 0 ? "Cleared all files from the chat." : "Dropped.";
        ctx.append(systemNote(`${head}\n\n${formatManifest(manifest)}`));
        ctx.notify("Chat files", head, "success");
      } catch (e) {
        ctx.notify("Drop from chat failed", humanizeError(e), "error");
      }
    },
  },
  {
    // Aider's `/ls`: list the files currently in the chat manifest.
    name: "ls",
    aliases: ["files", "manifest"],
    description: "List the files currently in the chat",
    run: async (_args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Chat files", "Open a project first.", "warning");
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const manifest = await invoke<ManifestEntry[]>("get_manifest", { projectRoot: root });
        ctx.append(systemNote(formatManifest(manifest)));
      } catch (e) {
        ctx.notify("Chat files failed", humanizeError(e), "error");
      }
    },
  },
  {
    // OpenHands-style knowledge microagents: list the keyword-triggered knowledge
    // files the active project ships under `.cortex/microagents/` (or
    // `.openhands/microagents/`). Each is injected into context automatically when
    // one of its trigger words appears in a message — this just shows what's defined.
    name: "microagents",
    aliases: ["knowledge"],
    description: "List the project's keyword-triggered knowledge microagents",
    run: async (_args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Microagents", "Open a project first.", "warning");
        return;
      }
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const agents = await invoke<MicroAgentInfo[]>("list_microagents", { projectRoot: root });
        ctx.append(systemNote(formatMicroAgents(agents)));
      } catch (e) {
        ctx.notify("Microagents failed", humanizeError(e), "error");
      }
    },
  },
  {
    // Quick rollback (aider's `/undo`): restore the most-recent workspace
    // checkpoint — typically the snapshot auto-taken right before `/apply`.
    // This overwrites the current working tree with that snapshot, so it
    // discards the applied edits (and any other changes made since the
    // checkpoint). Rather than force-restore blind, we first open the same
    // read-only diff preview the Checkpoints panel uses, so the user sees
    // exactly what would change before confirming. The panel still offers
    // picking an older checkpoint.
    name: "undo",
    aliases: ["rollback"],
    description: "Preview and undo the last /apply by restoring the most recent workspace checkpoint",
    run: async (_args, ctx) => {
      const root = ctx.store.getState().activeProject?.root;
      if (!root) {
        ctx.notify("Undo", "Open a project first.", "warning");
        return;
      }
      try {
        // list_checkpoints returns newest-first; the most recent is the undo target.
        const all = await listCheckpoints(root);
        const latest = all[0];
        if (!latest) {
          ctx.notify("Nothing to undo", "No workspace checkpoints found for this project.", "info");
          return;
        }
        const when = new Date(latest.ts).toLocaleString();
        const label = latest.label ? ` (“${latest.label}”)` : "";
        // Read-only preview → restore only on explicit confirm.
        const res = await reviewCheckpointRestore(root, latest);
        if (res.outcome === "cancelled") {
          ctx.notify("Undo cancelled", "Your working tree was left unchanged.", "info");
          return;
        }
        if (res.outcome === "error") {
          ctx.notify("Undo failed", res.message, "error");
          return;
        }
        ctx.append(
          systemNote(
            `↩ **Restored checkpoint**${label} from ${when} — ${latest.file_count} files. The working tree was rolled back to that snapshot.`,
          ),
        );
        ctx.notify("Undo complete", `Restored snapshot${label} from ${when}.`, "success");
      } catch (e) {
        ctx.notify("Undo failed", humanizeError(e), "error");
      }
    },
  },
  {
    name: "notify",
    description: "Fire an OS desktop notification (for smoke-testing the task-complete hook)",
    usage: "<title> <body>",
    run: async (args, ctx) => {
      const raw = args.trim();
      if (!raw) {
        ctx.notify("/notify skipped", "Usage: /notify <title> [body]", "warning");
        return;
      }
      // Split on the first run of whitespace — title is one word-ish chunk,
      // the body is everything that follows. Users who want spaces in the
      // title can quote it: `/notify "Build done" all green`.
      const quoted = raw.match(/^"([^"]+)"\s*(.*)$/);
      const [title, body] = quoted
        ? [quoted[1], quoted[2] ?? ""]
        : (() => {
            const idx = raw.search(/\s/);
            return idx < 0 ? [raw, ""] : [raw.slice(0, idx), raw.slice(idx + 1)];
          })();
      try {
        await desktopNotify(title, body);
        ctx.notify("Notified", `${title}${body ? ` — ${body}` : ""}`, "success");
      } catch (e) {
        ctx.append(errorNote(`/notify failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // ContextForge #13 — memory snapshots.
    //
    // Subcommands: `create [label]`, `list`, `rollback <id>`. The panel proper
    // lives behind the MemoryExplorer "📸 snapshots" button; this is the
    // keyboard-driven equivalent so power-users don't have to leave the chat.
    name: "snapshot",
    aliases: ["snap"],
    description: "Capture / list / restore point-in-time memory snapshots",
    usage: "create|list|rollback [label|id]",
    run: async (args, ctx) => {
      const { createSnapshot, listSnapshots, rollbackSnapshot, formatBytes, timeAgo } =
        await import("@/lib/snapshots");
      const parts = args.trim().split(/\s+/);
      const sub = (parts[0] || "list").toLowerCase();
      const rest = parts.slice(1).join(" ").trim();
      const project = ctx.store.getState().activeProject?.root ?? null;
      try {
        if (sub === "create" || sub === "new") {
          const label = rest || "manual";
          const meta = await createSnapshot(label, project);
          ctx.append(
            systemNote(
              `📸 snapshot **${meta.label}** captured — ${meta.file_count} files, ${formatBytes(meta.size_bytes)} (\`${meta.id}\`)`,
            ),
          );
          return;
        }
        if (sub === "list" || sub === "ls") {
          const items = await listSnapshots();
          if (items.length === 0) {
            ctx.append(systemNote("📸 no snapshots yet — `/snapshot create <label>` to capture one."));
            return;
          }
          const lines = items.slice(0, 20).map(
            (s) =>
              `- \`${s.id}\` · **${s.label}** · ${timeAgo(s.created_unix_ms)} · ${s.file_count} files · ${formatBytes(s.size_bytes)}`,
          );
          ctx.append(systemNote(`📸 snapshots (${items.length}):\n\n${lines.join("\n")}`));
          return;
        }
        if (sub === "rollback" || sub === "restore") {
          if (!rest) {
            ctx.notify("/snapshot rollback", "Usage: /snapshot rollback <id>", "warning");
            return;
          }
          if (
            !(await confirmDialog({
              title: "Roll back snapshot?",
              message: `Roll back snapshot ${rest}? Files newer than the snapshot will be preserved.`,
              confirmLabel: "Roll back",
              danger: true,
            }))
          )
            return;
          const report = await rollbackSnapshot(rest);
          ctx.append(
            systemNote(
              `📸 rollback complete — restored ${report.files_restored}, skipped ${report.files_skipped}${report.errors.length ? `, ${report.errors.length} error(s)` : ""}.`,
            ),
          );
          return;
        }
        ctx.notify("/snapshot", "Usage: /snapshot create|list|rollback", "warning");
      } catch (e) {
        ctx.append(errorNote(`/snapshot failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // PRP (Product Requirement Prompt) — staged feature spec backed by
    // `.cortex/prps/<name>.md`. `/prp list` jumps to the panel; `/prp create
    // <name>` writes a fresh stage-1 file and opens it.
    name: "prp",
    description: "Manage staged feature specs (PRPs) in the active project",
    usage: "list|create <name>",
    run: async (args, ctx) => {
      const raw = args.trim();
      const state = ctx.store.getState();
      if (!raw || raw === "list") {
        state.setActivityTab("prp");
        return;
      }
      const createMatch = raw.match(/^create\s+(\S+)\s*$/);
      if (!createMatch) {
        ctx.notify("/prp", "Usage: /prp list | /prp create <name>", "warning");
        return;
      }
      const name = createMatch[1];
      const projectRoot = state.activeProject?.root;
      if (!projectRoot) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      try {
        const prp = await createPrp(projectRoot, name);
        ctx.append(systemNote(`📐 created PRP **${prp.name}** at \`${prp.path}\``));
        state.setActivityTab("prp");
      } catch (e) {
        ctx.append(errorNote(`/prp create failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Deep Research hand-off (P0-FINAL flow item). Bare `/research` opens the
    // panel; with a question it starts the run immediately. The run lives in
    // the global job store (state/jobs.ts), so it doesn't matter whether the
    // panel is mounted yet — it renders whatever the store is doing when it
    // appears.
    name: "research",
    aliases: ["deep-research"],
    description: "Open Deep Research — with a question, run it immediately",
    usage: "[question]",
    run: async (args, ctx) => {
      const q = args.trim();
      ctx.store.getState().setActivityTab("research");
      if (q) {
        const { startDeepResearch } = await import("@/state/jobs");
        void startDeepResearch(q);
      }
    },
  },
  {
    // ContextForge #5 — multi-IDE config export.
    //
    // Opens a portal modal (no App.tsx wiring) listing Cursor/Windsurf/Cline/
    // Copilot/Codex. The modal calls `export_ide_configs` against the active
    // project and renders a per-format written/skipped breakdown.
    name: "export-ide",
    aliases: ["ide-export", "ide"],
    description: "Export merged CLAUDE.md/AGENTS.md/.cortex/rules to IDE rule files",
    run: async (_a, ctx) => {
      try {
        // Dynamic import keeps the export modal out of the main bundle until
        // the user actually summons it.
        const { openIDEExportModal } = await import("@/components/IDEExportModal");
        openIDEExportModal();
      } catch (e) {
        ctx.append(errorNote(`/export-ide failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // ContextForge #9 — encrypted provider key vault.
    //
    // Lives as a modal (same portal pattern as /export-ide) because
    // ActivityPanel.tsx is intentionally untouched in this change set. The
    // panel component is structured so it can later be hosted as a tab with
    // no logic changes.
    name: "vault",
    aliases: ["keys", "keyvault"],
    description: "Open the encrypted provider key vault",
    run: async (_a, ctx) => {
      try {
        const { openKeyVaultPanel } = await import("@/components/KeyVaultPanel");
        openKeyVaultPanel();
      } catch (e) {
        ctx.append(errorNote(`/vault failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Audit log viewer — modal portal over the `recent_audit` command.
    // Same pattern as /vault and /export-ide so App.tsx stays untouched.
    name: "audit",
    aliases: ["log", "auditlog"],
    description: "Open the agent audit log viewer",
    run: async (_a, ctx) => {
      try {
        const { openAuditLogPanel } = await import("@/components/AuditLogPanel");
        openAuditLogPanel();
      } catch (e) {
        ctx.append(errorNote(`/audit failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Outbound webhook manager — list/add/edit/test entries persisted at
    // `~/.cortex/webhooks.json`. ContextForge #14.
    name: "webhook",
    // Note: no `hooks` alias here — that key belongs to the read-only hooks
    // inspector command above; aliasing it would shadow the inspector's
    // `/hooks` binding when INDEX is built in command order.
    aliases: ["webhooks"],
    description: "Manage outbound webhook subscriptions",
    run: async (_a, ctx) => {
      try {
        const { openWebhooksPanel } = await import("@/components/WebhooksPanel");
        openWebhooksPanel();
      } catch (e) {
        ctx.append(errorNote(`/webhook failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // ContextForge #7 — apply a pre-built persona to the currently-active
    // agent. The role's `system_prompt` is piped into the per-agent custom
    // instructions store, which the chat pipeline already prepends to the
    // outgoing system prompt. Keyboard-driven equivalent of the per-row
    // "Apply" button in RolesPanel.
    name: "role",
    description: "Apply a pre-built role to the active agent",
    usage: "<name>",
    run: async (args, ctx) => {
      const name = args.trim();
      if (!name) {
        ctx.notify("/role", "Usage: /role <name>", "warning");
        return;
      }
      const agents = ctx.store.getState().agents;
      // No explicit "current agent" in the store today — pick the first
      // available descriptor, falling back to the first overall. The Apply
      // button in RolesPanel exposes the per-agent override for power users.
      const target = agents.find((a) => a.available)?.id ?? agents[0]?.id;
      if (!target) {
        ctx.append(errorNote("/role: no agents available yet."));
        return;
      }
      try {
        const { applyRoleToAgent } = await import("@/lib/roles");
        await applyRoleToAgent(name, target);
        ctx.append(systemNote(`🎭 role **${name}** applied to \`${target}\``));
      } catch (e) {
        ctx.append(errorNote(`/role failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // ContextForge #11 — schema-locked settings.json editor.
    //
    // Opens a portal modal (same pattern as /export-ide and /vault) wired to
    // the `config_files` backend. Pass an optional preset id to jump
    // straight to that file: `/edit-config snippets`,
    // `/edit-config trust-matrix`, etc. Unknown ids fall back to the default
    // (snippets) so the user still lands on a working editor.
    name: "edit-config",
    aliases: ["config", "settings-edit"],
    description: "Open the schema-locked Cortex config editor",
    usage: "[name]",
    run: async (args, ctx) => {
      const name = args.trim() || undefined;
      try {
        const { openSchemaEditor } = await import("@/components/SchemaEditor");
        openSchemaEditor(name);
      } catch (e) {
        ctx.append(errorNote(`/edit-config failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Multi-agent orchestrator dashboard (ContextForge #8).
    //
    // Subcommands: `list` opens the panel, `status [name]` prints a one-shot
    // text summary in the chat (handy when the panel is collapsed), `create
    // <name>` opens the panel with the New-team modal in scope. Anything else
    // falls through to the panel.
    name: "team",
    aliases: ["teams", "orchestrator"],
    description: "Manage multi-agent teams in the orchestrator dashboard",
    usage: "list|create|status [name]",
    run: async (args, ctx) => {
      const parts = args.trim().split(/\s+/);
      const sub = (parts[0] || "list").toLowerCase();
      const rest = parts.slice(1).join(" ").trim();
      const state = ctx.store.getState();
      if (sub === "list" || sub === "" || sub === "create") {
        state.setActivityTab("orchestrator");
        return;
      }
      if (sub === "status") {
        try {
          const { listTeams, timeAgo } = await import("@/lib/teams");
          const all = await listTeams();
          if (all.length === 0) {
            ctx.append(systemNote("🛰️ no teams yet — `/team create` to spin one up."));
            return;
          }
          const teams = rest
            ? all.filter((t) => t.name.toLowerCase() === rest.toLowerCase())
            : all;
          if (teams.length === 0) {
            ctx.append(errorNote(`/team status: no team named '${rest}'.`));
            return;
          }
          const lines: string[] = [];
          for (const t of teams) {
            lines.push(
              `**${t.name}** — manager \`${t.manager_role}\` · ${t.workers.length} worker(s) · ${timeAgo(t.created_unix_ms)}`,
            );
            for (const w of t.workers) {
              lines.push(
                `  - \`${w.role}\` · _${w.status}_ · ${w.current_task ?? "—"} · ${w.message_count} msg`,
              );
            }
          }
          ctx.append(systemNote(`🛰️ teams (${teams.length}):\n\n${lines.join("\n")}`));
          return;
        } catch (e) {
          ctx.append(errorNote(`/team status failed: ${humanizeError(e)}`));
          return;
        }
      }
      ctx.notify("/team", "Usage: /team list | /team create | /team status [name]", "warning");
    },
  },
  tabCmd("tools", "tools", "Jump to the REST→MCP tool registry", ["tool"]),
  tabCmd("snippets", "snippets", "Jump to the saved-prompt snippets panel", ["snippet"]),
  {
    // AI commit-message suggester. Reads the staged diff (falls back to
    // unstaged) from the active project, asks the gateway for a Conventional
    // Commits-style message, then copies it to the clipboard so the user can
    // paste it into their git client of choice.
    name: "commit-msg",
    aliases: ["commit", "commitmsg"],
    description: "Generate a Conventional Commits message for the active project's diff",
    run: async (_a, ctx) => {
      const project = ctx.store.getState().activeProject;
      if (!project) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      ctx.append(systemNote(`✍️ generating commit message for **${project.name}**…`));
      try {
        const { suggestCommitMessage } = await import("@/lib/commit-suggest");
        const msg = await suggestCommitMessage(project.root);
        try {
          await navigator.clipboard.writeText(msg);
          ctx.notify("Commit message copied", msg.split("\n")[0] ?? "", "success");
        } catch {
          // Clipboard can fail in dev (no user gesture, denied permission, …)
          // — still surface the message in chat so the user can grab it.
          ctx.notify("Generated (clipboard unavailable)", msg.split("\n")[0] ?? "", "info");
        }
        ctx.append(systemNote("```\n" + msg + "\n```"));
      } catch (e) {
        ctx.append(errorNote(`/commit-msg failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // `/share` — render the chat as markdown. No-arg form copies to clipboard;
    // when a filename is supplied we write it to
    // `~/Documents/Cortex Brain/shared/<name>.md` (the `.md` suffix is added
    // when missing). The backend enforces the same root for path safety.
    name: "share",
    description: "Export this chat as markdown (clipboard, or a file under Cortex Brain/shared)",
    usage: "[filename]",
    run: async (args, ctx) => {
      const state = ctx.store.getState();
      const messages = state.messages;
      if (messages.length === 0) {
        ctx.notify("/share skipped", "No messages to export.", "info");
        return;
      }
      // Strip ToolEvent / approval / runId — backend only needs the bits that
      // render in markdown, and ShareMessage explicitly defines that subset.
      const payload = messages.map((m) => ({
        role: m.role,
        agent: m.agent ?? null,
        content: m.content,
        ts_unix_ms: null,
      }));
      const projectRoot = state.activeProject?.root ?? null;
      const name = args.trim();
      try {
        const { shareChatAsMarkdown } = await import("@/lib/share");
        if (!name) {
          const md = await shareChatAsMarkdown(payload, null, projectRoot);
          try {
            await navigator.clipboard.writeText(md);
            ctx.notify("Chat copied", `${messages.length} messages as markdown.`, "success");
          } catch (e) {
            ctx.notify("Copy failed", humanizeError(e), "error");
          }
          return;
        }
        const { homeDir, join } = await import("@tauri-apps/api/path");
        const home = await homeDir();
        const filename = /\.md$/i.test(name) ? name : `${name}.md`;
        const target = await join(home, "Documents", "Cortex Brain", "shared", filename);
        await shareChatAsMarkdown(payload, target, projectRoot);
        ctx.append(systemNote(`📤 chat saved to \`${target}\``));
        ctx.notify("Chat shared", filename, "success");
      } catch (e) {
        ctx.append(errorNote(`/share failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Help panel + onboarding tour. Appended AFTER the older `help` entry so
    // the INDEX builder's `Map.set` overwrites the shortcuts-cheat-sheet
    // behaviour and routes both `/help` and `/tour` here. `/help` jumps to
    // the Help activity tab; `/tour` re-fires the 5-step feature tour
    // regardless of the user's `onboardingComplete` flag.
    name: "help",
    // Note: appended AFTER the earlier "shortcuts cheat sheet" `help` entry
    // so the INDEX builder's `Map.set` overrides that older behaviour with
    // this new "open the Help panel" handler.
    description: "Open the Help panel",
    run: (_a, ctx) => {
      ctx.store.getState().setActivityTab("help");
    },
  },
  {
    name: "tour",
    description: "Re-launch the 5-step Cortex feature tour",
    run: async (_a, ctx) => {
      // Open the Help panel so the tour card has somewhere on-screen to
      // anchor visually, then fire the tour trigger — the OnboardingTour
      // component listens for this and resets itself to step 0 regardless
      // of the user's `onboardingComplete` flag.
      ctx.store.getState().setActivityTab("help");
      try {
        const { triggerTour } = await import("@/lib/onboarding");
        triggerTour();
      } catch {
        /* tour module failed to load — Help panel is still open, no-op. */
      }
    },
  },
  {
    // Unified search across both universes — project files AND memory/vault.
    // Opens the Search activity panel; any `args` string is treated as a
    // pre-seeded query that the panel picks up via `setSearchPreload` before
    // mount. The "Everything" scope fans the query out to both universes;
    // the panel also offers Project-only, Memory-only, and "Go to file"
    // (fuzzy path) scopes.
    //
    // `/find` aliases the same command so users coming from Sublime/Vim
    // muscle memory land in the same place.
    name: "search",
    aliases: ["find"],
    description: "Search across project files and memory/vault",
    usage: "[query]",
    run: async (args, ctx) => {
      const q = args.trim();
      try {
        const { setSearchPreload } = await import("@/components/SearchPanel");
        setSearchPreload({ preload: q, mode: "all" });
      } catch {
        /* SearchPanel module failed to load — fall through to opening the
         * panel anyway so the user at least sees the empty state. */
      }
      ctx.store.getState().setActivityTab("search");
    },
  },
  tabCmd("gateway", "gateway", "Jump to the Cortex Gateway models + capabilities panel", ["caps"]),
  {
    // Workflow templates — preset multi-step recipes at
    // `~/.cortex/workflows/<name>.yaml`. `/workflow <name>` launches the
    // workflow (one chat message per step with a `[role:…]` prefix);
    // `/wf` (alias) with no arg jumps to the panel for browse/edit.
    name: "workflow",
    aliases: ["wf"],
    description: "Run a preset multi-step workflow, or list them",
    usage: "[name]",
    run: async (args, ctx) => {
      const name = args.trim();
      if (!name) {
        ctx.store.getState().setActivityTab("workflows");
        return;
      }
      try {
        const { runWorkflow, formatStepPrompt } = await import("@/lib/workflows");
        const run = await runWorkflow(name);
        if (!run) {
          ctx.append(errorNote(`/workflow: '${name}' not found.`));
          return;
        }
        ctx.append(
          systemNote(
            `▶︎ workflow **${run.name}** queued — ${run.steps.length} step${
              run.steps.length === 1 ? "" : "s"
            } (\`${run.run_id}\`)`,
          ),
        );
        run.steps.forEach((step, idx) => {
          ctx.append(
            systemNote(
              `**Step ${idx + 1}/${run.steps.length}** · ${formatStepPrompt(step)}`,
            ),
          );
        });
      } catch (e) {
        ctx.append(errorNote(`/workflow failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Save the current chat into the Cortex Brain vault as a markdown file
    // under `~/Documents/Cortex Brain/imports/<date>-<slug>.md`. Distinct from
    // `/share` (which targets `shared/`) — `/brain-save` flags the file as an
    // import so downstream memory walkers and the Brain panel surface it as
    // ingested context rather than an outbound export.
    name: "brain-save",
    aliases: ["brainsave", "save-brain"],
    description: "Save this chat into the Cortex Brain vault under imports/",
    usage: "[label]",
    run: async (args, ctx) => {
      const messages = ctx.store.getState().messages;
      if (messages.length === 0) {
        ctx.notify("/brain-save skipped", "No messages to save.", "info");
        return;
      }
      // Default label: first user message's first 60 chars. Falls back to a
      // timestamped name when there's no user message yet (e.g. assistant-only
      // bootstrap transcript).
      const firstUser = messages.find((m) => m.role === "user");
      const fallback = (firstUser?.content ?? "import").slice(0, 60).trim();
      const label = args.trim() || fallback || `chat-${Date.now()}`;
      const body = messages
        .map((m) => `### ${m.agent ?? m.role}\n\n${m.content}\n`)
        .join("\n");
      try {
        const { importToBrain } = await import("@/lib/brain-import");
        const result = await importToBrain(body, label, "chat");
        ctx.append(
          systemNote(
            `🧠 saved to \`${result.written_path}\` (${result.bytes} bytes)`,
          ),
        );
        ctx.notify("Saved to Brain", label, "success");
      } catch (e) {
        ctx.append(errorNote(`/brain-save failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Open the memory-dedupe modal. Mirrors `/export-ide` / `/vault` —
    // self-mounting portal so we don't touch App.tsx wiring. Surfaces
    // Jaccard-similar markdown pairs across every memory source and lets the
    // user open either file in the editor pane to reconcile them.
    name: "dedupe",
    aliases: ["duplicates", "dupes"],
    description: "Scan memory sources for near-duplicate markdown files",
    run: async (_a, ctx) => {
      try {
        const { openDedupePanel } = await import("@/components/DedupePanel");
        openDedupePanel();
      } catch (e) {
        ctx.append(errorNote(`/dedupe failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Ctrl+P-style recent-files / path fuzzy-find quick-open. Self-mounting
    // portal modal so we don't have to touch App.tsx wiring; the global
    // shortcut binding is a follow-up. `/open <fragment>` pre-fills the
    // search box so users can type the whole thing in one shot.
    name: "open",
    aliases: ["p"],
    description: "Quick-open a recent or matching file (Ctrl+P-style picker)",
    usage: "[fragment]",
    run: async (args, ctx) => {
      try {
        const { openQuickOpen } = await import("@/lib/quick-open");
        openQuickOpen(args.trim() || undefined);
      } catch (e) {
        ctx.append(errorNote(`/open failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Sentry-style crash viewer over the `recent_crashes` Tauri command. Same
    // self-mounting portal pattern as `/audit` / `/vault` so App.tsx stays
    // untouched. Surfaces both Rust panics and JS errors with severity, kind,
    // location, stack, version and OS; "Copy as JSON" / "Replay last user
    // message" actions are per-row.
    name: "crashes",
    aliases: ["crash"],
    description: "Open the crash report viewer (Rust panics + JS errors)",
    run: async (_a, ctx) => {
      try {
        const { openCrashViewer } = await import("@/lib/crash-viewer");
        await openCrashViewer();
      } catch (e) {
        ctx.append(errorNote(`/crashes failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Guided new-memory wizard. Self-mounting portal modal (same pattern as
    // `/export-ide` / `/vault`) so App.tsx stays untouched. The optional arg
    // pre-fills the title field so users can type the slug in one shot —
    // `/new-memory cortex-roadmap` lands them on the form with the title set.
    name: "new-memory",
    aliases: ["memnew"],
    description: "Open the guided new-memory entry wizard",
    usage: "[title]",
    run: async (args, ctx) => {
      const title = args.trim() || undefined;
      try {
        const { openMemoryWizard } = await import("@/lib/memory-wizard");
        await openMemoryWizard(title);
      } catch (e) {
        ctx.append(errorNote(`/new-memory failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Full Cortex backup + restore. Tarballs land at
    // `~/.cortex/backups/<unix_ms>-<label>.tar.gz` and include every
    // ~/.cortex/* user config plus Claude project memory `.md` files.
    // Self-mounting portal modal, same pattern as `/export-ide` / `/vault`.
    name: "backup",
    aliases: ["backups"],
    description: "Open the full backup + restore panel",
    run: async (_a, ctx) => {
      try {
        const { openBackupPanel } = await import("@/components/BackupPanel");
        openBackupPanel();
      } catch (e) {
        ctx.append(errorNote(`/backup failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // `/restore` jumps to the backup panel pre-focused on the most recent
    // entry — same modal as `/backup`, just with a focusLatest flag set.
    name: "restore",
    description: "Open the backup panel pre-focused on the most recent backup",
    run: async (_a, ctx) => {
      try {
        const { openBackupPanel } = await import("@/components/BackupPanel");
        openBackupPanel({ focusLatest: true });
      } catch (e) {
        ctx.append(errorNote(`/restore failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Memory-bridge stats panel — health/state of every memory source plus
    // the claude-mem chroma DB. Same self-mounting portal pattern as
    // `/export-ide`, `/vault`, `/backup`, etc. so App.tsx stays untouched.
    name: "memstats",
    aliases: ["mem-stats", "memory-stats"],
    description: "Open the memory-bridge stats panel",
    run: async (_a, ctx) => {
      try {
        const { openMemoryStatsPanel } = await import("@/components/MemoryStatsPanel");
        openMemoryStatsPanel();
      } catch (e) {
        ctx.append(errorNote(`/memstats failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Keyboard shortcut for the "Sync now" button in `/memstats`. Fires the
    // `sync_memory` backend (which delegates to `import_claude_mem`) and
    // surfaces the count via a toast so the user gets feedback without having
    // to open the panel.
    name: "sync",
    aliases: ["memsync", "memory-sync"],
    description: "Sync external memory sources into Cortex's imported store",
    run: async (_a, ctx) => {
      try {
        const { syncMemory } = await import("@/lib/memory-stats");
        const report = await syncMemory();
        const kind =
          report.errors.length === 0
            ? report.imported > 0
              ? "success"
              : "info"
            : "warning";
        ctx.notify(
          "Memory sync complete",
          `${report.imported} imported, ${report.skipped} skipped${
            report.errors.length ? `, ${report.errors.length} error(s)` : ""
          }.`,
          kind,
        );
      } catch (e) {
        ctx.append(errorNote(`/sync failed: ${humanizeError(e)}`));
      }
    },
  },
  tabCmd(
    "today",
    "today",
    "Jump to the Today activity dashboard (focus chain, PRPs, recent crashes, today's tokens)",
  ),
  {
    // ContextForge #4 — Spaces. Scoped subsets of a project (frontend /
    // backend / docs / …) defined by glob include/exclude lists at
    // `<project>/.cortex/spaces.yaml`. Same self-mounting portal pattern as
    // `/export-ide`, `/vault`, etc. — no App.tsx wiring needed.
    name: "spaces",
    description: "Open the Spaces panel (scoped project subsets via globs)",
    run: async (_a, ctx) => {
      try {
        const { openSpacesPanel } = await import("@/components/SpacesPanel");
        openSpacesPanel();
      } catch (e) {
        ctx.append(errorNote(`/spaces failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // `/space <name>` — alias that jumps straight into the browse view for a
    // named space. No arg falls back to the same panel as `/spaces`.
    name: "space",
    description: "Browse a named space (alias for /spaces, accepts a space name)",
    usage: "[name]",
    run: async (args, ctx) => {
      const name = args.trim();
      try {
        const { openSpacesPanel } = await import("@/components/SpacesPanel");
        openSpacesPanel(name || undefined);
      } catch (e) {
        ctx.append(errorNote(`/space failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // `/stage <intent>` — AI-guided git staging. the gateway reads the working
    // diff + status and picks which files to `git add` based on the user's
    // free-form intent. The backend is conservative by design: when the
    // intent is ambiguous, it stages nothing rather than guessing wrong.
    name: "stage",
    description: "AI-guided git staging — pick files to stage by free-form intent",
    usage: "<intent>",
    run: async (args, ctx) => {
      const intent = args.trim();
      const project = ctx.store.getState().activeProject;
      if (!project) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      if (!intent) {
        ctx.append(errorNote("/stage needs an intent, e.g. `/stage just the backend changes`."));
        return;
      }
      ctx.append(systemNote(`🪄 staging in **${project.name}**…`));
      try {
        const { smartStage } = await import("@/lib/smart-stage");
        const report = await smartStage(project.root, intent);
        const lines: string[] = [`**${report.reason}**`];
        if (report.staged.length > 0) {
          lines.push("", "Staged:");
          for (const p of report.staged) lines.push(`- \`${p}\``);
        } else {
          lines.push("", "_No files staged._");
        }
        if (report.skipped.length > 0) {
          lines.push("", "Skipped:");
          for (const p of report.skipped) lines.push(`- \`${p}\``);
        }
        if (report.errors.length > 0) {
          lines.push("", "Errors:");
          for (const e of report.errors) lines.push(`- ${e}`);
        }
        ctx.append(systemNote(lines.join("\n")));
        ctx.notify(
          "/stage complete",
          `${report.staged.length} staged, ${report.skipped.length} skipped`,
          report.errors.length > 0 ? "warning" : "success",
        );
      } catch (e) {
        ctx.append(errorNote(`/stage failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // AI session summarizer — replays the current session's message history
    // through the gateway and renders a headline + bullet body inside a portal
    // modal. "Save to brain" persists it to `~/Documents/Cortex Brain/
    // sessions/<session_id>-summary.md` with YAML frontmatter.
    name: "summary",
    aliases: ["summarize", "summarise"],
    description: "Summarise the current session via AI (headline + bullets + next steps)",
    run: async (_a, ctx) => {
      const sessionId = ctx.store.getState().sessionId;
      if (!sessionId) {
        ctx.notify("/summary skipped", "No active session yet.", "warning");
        return;
      }
      try {
        const { openSessionSummaryModal } = await import("@/components/SessionSummaryModal");
        openSessionSummaryModal(sessionId);
      } catch (e) {
        ctx.append(errorNote(`/summary failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // Cost tracker — shows the running USD estimate for the current session,
    // priced from the hardcoded 2026-defaults table in
    // `commands/cost_tracker.rs`. Surfaced as a toast so it stays out of the
    // chat scroll; for a fuller breakdown the Usage panel will gain a tab.
    name: "cost",
    aliases: ["spend", "usd"],
    description: "Show estimated USD spend for the current session",
    run: async (_a, ctx) => {
      const sessionId = ctx.store.getState().sessionId;
      try {
        const { estimateCost, formatUsd } = await import("@/lib/cost-tracker");
        const report = await estimateCost(sessionId || undefined);
        const row = report.by_session[0];
        if (!row && sessionId) {
          ctx.notify(
            "No cost data yet",
            "This session hasn't completed any priced runs.",
            "info",
          );
          return;
        }
        const total = formatUsd(report.total_usd);
        const tokenStr = row
          ? ` · ${row.total_tokens.toLocaleString("en-US")} tokens`
          : "";
        const modelTop = report.by_model[0];
        const modelStr = modelTop
          ? ` · top model \`${modelTop.model}\` ${formatUsd(modelTop.usd)}`
          : "";
        ctx.notify(
          `Estimated cost: ${total}`,
          `${sessionId ? "this session" : "all sessions"}${tokenStr}${modelStr}`,
          "info",
        );
      } catch (e) {
        ctx.append(errorNote(`/cost failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    name: "budget",
    aliases: ["cap", "limit"],
    description: "Set or check the USD spend cap — warns at 80%, confirms sends past 100%",
    usage: "[<usd> | off]",
    run: async (args, ctx) => {
      const arg = args.trim().toLowerCase();
      const {
        getBudgetCap,
        setBudgetCap,
        clearBudgetCap,
        budgetLevel,
      } = await import("@/lib/budget");

      if (arg === "off" || arg === "clear" || arg === "none") {
        clearBudgetCap();
        ctx.notify("Budget cleared", "No spend cap is set.", "info");
        return;
      }

      if (arg) {
        const usd = Number.parseFloat(arg.replace(/^\$/, ""));
        if (!Number.isFinite(usd) || usd <= 0) {
          ctx.notify("Budget", "Usage: /budget <usd> · /budget off", "warning");
          return;
        }
        if (!setBudgetCap(usd)) {
          ctx.notify("Budget", "Could not save the cap (storage unavailable).", "error");
          return;
        }
        const { formatUsd } = await import("@/lib/cost-tracker");
        ctx.notify("Budget set", `Cap: ${formatUsd(usd)} — warns at 80%, sends past 100% ask to confirm.`, "success");
        return;
      }

      // Bare `/budget` → report spend vs cap.
      const cap = getBudgetCap();
      const sessionId = ctx.store.getState().sessionId;
      try {
        const { estimateCost, formatUsd } = await import("@/lib/cost-tracker");
        const report = await estimateCost(sessionId || undefined);
        const spent = report.total_usd;
        if (cap === null) {
          ctx.notify(
            `Spend: ${formatUsd(spent)}`,
            "No cap set — use `/budget <usd>` to set one.",
            "info",
          );
          return;
        }
        const pct = Math.round((spent / cap) * 100);
        const level = budgetLevel(spent, cap);
        const kind = level === "over" ? "error" : level === "warn" ? "warning" : "success";
        const lead =
          level === "over"
            ? "Over budget"
            : level === "warn"
              ? "Approaching budget"
              : "Within budget";
        ctx.notify(
          `${lead}: ${formatUsd(spent)} / ${formatUsd(cap)}`,
          `${pct}% of the ${sessionId ? "session" : "total"} cap used.`,
          kind,
        );
      } catch (e) {
        ctx.append(errorNote(`/budget failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // `/commit [intent]` — full AI commit pipeline.
    //
    // Optional intent → `smart_stage` first to AI-pick which files to add.
    // Then `suggest_commit_message` against the staged diff, show the result
    // as a system note, and finally `git_commit_staged` to commit (hooks
    // still run; no `--no-verify`). Appended AFTER the earlier "commit-msg"
    // entry so the INDEX builder's `Map.set` overrides its `commit` alias
    // with this end-to-end flow.
    name: "commit",
    description: "AI-stage (optional) + AI-message + commit in one shot",
    usage: "[intent]",
    run: async (args, ctx) => {
      const intent = args.trim();
      const project = ctx.store.getState().activeProject;
      if (!project) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      try {
        if (intent) {
          ctx.append(systemNote(`🪄 staging in **${project.name}** — _${intent}_…`));
          const { smartStage } = await import("@/lib/smart-stage");
          const stageReport = await smartStage(project.root, intent);
          const lines: string[] = [`**${stageReport.reason}**`];
          if (stageReport.staged.length > 0) {
            lines.push("", "Staged:");
            for (const p of stageReport.staged) lines.push(`- \`${p}\``);
          } else {
            lines.push("", "_No files staged — falling back to existing index._");
          }
          if (stageReport.skipped.length > 0) {
            lines.push("", "Skipped:");
            for (const p of stageReport.skipped) lines.push(`- \`${p}\``);
          }
          ctx.append(systemNote(lines.join("\n")));
        }
        ctx.append(systemNote(`✍️ generating commit message for **${project.name}**…`));
        const { suggestCommitMessage } = await import("@/lib/commit-suggest");
        const message = await suggestCommitMessage(project.root);
        ctx.append(systemNote("Proposed commit message:\n\n```\n" + message + "\n```"));
        const { gitCommitStaged } = await import("@/lib/git-push");
        await gitCommitStaged(project.root, message);
        ctx.append(systemNote(`✓ committed: ${message.split("\n")[0] ?? "(no subject)"}`));
        ctx.notify("/commit complete", message.split("\n")[0] ?? "", "success");
      } catch (e) {
        ctx.append(errorNote(`/commit failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // `/push [branch] [--force]` — push the current branch (or a named one)
    // to `origin`. Force is opt-in via `--force` so a stray `/push` never
    // rewrites remote history. The branch arg is optional; with neither arg
    // we push `HEAD`.
    name: "push",
    description: "Push to origin (no force unless --force is passed)",
    usage: "[branch] [--force]",
    run: async (args, ctx) => {
      const project = ctx.store.getState().activeProject;
      if (!project) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      const tokens = args.trim().split(/\s+/).filter(Boolean);
      let force = false;
      const positional: string[] = [];
      for (const t of tokens) {
        if (t === "--force" || t === "-f") force = true;
        else positional.push(t);
      }
      const branch = positional[0] ?? null;
      ctx.append(systemNote(`🚀 pushing **${project.name}** → \`${branch ?? "HEAD"}\`${force ? " _(force)_" : ""}…`));
      try {
        const { gitPush, summarizePushResult } = await import("@/lib/git-push");
        const result = await gitPush(project.root, branch, force);
        const lead = result.ok ? "✓ push ok" : `✗ push failed (exit ${result.exit_code})`;
        const summary = summarizePushResult(result);
        ctx.append(systemNote(`${lead} — \`${result.branch}\`\n\n\`\`\`\n${summary}\n\`\`\``));
        ctx.notify(
          result.ok ? "/push ok" : "/push failed",
          `${result.branch}: ${summary}`,
          result.ok ? "success" : "error",
        );
      } catch (e) {
        ctx.append(errorNote(`/push failed: ${humanizeError(e)}`));
      }
    },
  },
  {
    // `/ship [intent]` — the killer flow. `smart_stage` (when intent given)
    // → `suggest_commit_message` → `git_commit_staged` → `git_push`, with a
    // final summary note. Halts + posts an error note on the first failure.
    name: "ship",
    description: "AI-stage + AI-commit + push in one shot (the killer flow)",
    usage: "[intent]",
    run: async (args, ctx) => {
      const intent = args.trim();
      const project = ctx.store.getState().activeProject;
      if (!project) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      try {
        let stagedCount = 0;
        if (intent) {
          ctx.append(systemNote(`🪄 [1/4] staging in **${project.name}** — _${intent}_…`));
          const { smartStage } = await import("@/lib/smart-stage");
          const stageReport = await smartStage(project.root, intent);
          stagedCount = stageReport.staged.length;
          const lines: string[] = [`**${stageReport.reason}**`];
          if (stageReport.staged.length > 0) {
            lines.push("", "Staged:");
            for (const p of stageReport.staged) lines.push(`- \`${p}\``);
          } else {
            lines.push("", "_No files staged — using existing index._");
          }
          if (stageReport.skipped.length > 0) {
            lines.push("", "Skipped:");
            for (const p of stageReport.skipped) lines.push(`- \`${p}\``);
          }
          ctx.append(systemNote(lines.join("\n")));
        }

        ctx.append(systemNote(`✍️ [2/4] generating commit message…`));
        const { suggestCommitMessage } = await import("@/lib/commit-suggest");
        const message = await suggestCommitMessage(project.root);
        ctx.append(systemNote("Proposed commit message:\n\n```\n" + message + "\n```"));

        ctx.append(systemNote(`💾 [3/4] committing…`));
        const { gitCommitStaged, gitPush, summarizePushResult } = await import("@/lib/git-push");
        await gitCommitStaged(project.root, message);

        ctx.append(systemNote(`🚀 [4/4] pushing to origin…`));
        const result = await gitPush(project.root, null, false);
        if (!result.ok) {
          const summary = summarizePushResult(result);
          ctx.append(
            errorNote(
              `/ship halted at push step — commit landed locally but push failed (exit ${result.exit_code}):\n\n\`\`\`\n${summary}\n\`\`\``,
            ),
          );
          ctx.notify("/ship push failed", summary, "error");
          return;
        }

        const subject = message.split("\n")[0] ?? "(no subject)";
        const fileSummary = stagedCount > 0 ? `${stagedCount} file${stagedCount === 1 ? "" : "s"}` : "staged index";
        ctx.append(
          systemNote(
            `✓ shipped: \`${subject}\` · ${fileSummary} · pushed to \`${result.branch}\``,
          ),
        );
        ctx.notify("/ship complete", `${subject} → ${result.branch}`, "success");
      } catch (e) {
        ctx.append(errorNote(`/ship halted: ${humanizeError(e)}`));
        ctx.notify("/ship failed", humanizeError(e), "error");
      }
    },
  },
  {
    // Inline test runner. Auto-detects the project's framework (Cargo /
    // Vitest / Jest / Mocha / Pytest), runs it, and renders a portal modal
    // with passed/failed/skipped pills and a click-to-expand failure list.
    // Optional arg overrides detection: `/test cargo`, `/test pytest`, …
    name: "test",
    aliases: ["tests", "runtests"],
    description: "Run the project's test suite in a panel with parsed results",
    usage: "[framework]",
    run: async (args, ctx) => {
      const project = ctx.store.getState().activeProject;
      if (!project) {
        ctx.append(errorNote("No active project — pick one from the sidebar first."));
        return;
      }
      const fw = args.trim() || undefined;
      try {
        const { openTestRunnerPanel } = await import("@/components/TestRunnerPanel");
        openTestRunnerPanel(fw);
      } catch (e) {
        ctx.append(errorNote(`/test failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  // `/graph` (alias `/knowledge`) — jump to the wikilink knowledge-graph
  // panel. The panel itself walks every memory source on mount, so no args
  // are needed; future flags (e.g. `--source=runbooks` to scope) would
  // attach here without touching ActivityPanel.
  tabCmd(
    "graph",
    "knowledge-graph",
    "Open the wikilink knowledge-graph visualizer across all memory entries",
    ["knowledge"],
  ),
  {
    // AI refactor suggester. Asks the gateway for 3-5 specific refactors on the
    // target file (defaults to the currently-open editor path) and renders
    // them as expandable before/after cards in a portal modal — same self-
    // mounting pattern as `/summary` / `/test` so App.tsx stays untouched.
    // Arg form: `/refactor` (uses editorPath), `/refactor <path>`, or
    // `/refactor <path> :: <intent>` to pre-seed the focus input.
    name: "refactor",
    aliases: ["refac", "refactors"],
    description: "AI-propose specific refactors for a file (before/after, with rationale)",
    usage: "[path] [:: intent]",
    run: async (args, ctx) => {
      const raw = args.trim();
      let path = "";
      let intent: string | undefined;
      if (raw.includes("::")) {
        const [p, ...rest] = raw.split("::");
        path = (p ?? "").trim();
        const focus = rest.join("::").trim();
        if (focus) intent = focus;
      } else {
        path = raw;
      }
      if (!path) {
        path = ctx.store.getState().editorPath ?? "";
      }
      if (!path) {
        ctx.notify(
          "/refactor",
          "No file. Open one in the editor or pass a path: /refactor <path>.",
          "warning",
        );
        return;
      }
      try {
        const { openRefactorSuggesterModal } = await import(
          "@/components/RefactorSuggesterModal"
        );
        openRefactorSuggesterModal(path, intent);
      } catch (e) {
        ctx.append(errorNote(`/refactor failed to mount: ${humanizeError(e)}`));
      }
    },
  },
  {
    // AI documentation generator. Pipes the target file through the gateway with
    // a language-aware style prompt (rust/jsdoc/python/markdown/generic) and
    // shows the original vs documented version side-by-side in a portal
    // modal — same self-mounting pattern as `/refactor` / `/summary`. The
    // path arg defaults to the currently-open editor path so `/docgen` is a
    // one-keystroke ask in the common case.
    name: "docgen",
    aliases: ["docs"],
    description: "AI-generate idiomatic inline documentation for a file",
    usage: "[path]",
    run: async (args, ctx) => {
      const explicit = args.trim();
      const path = explicit || (ctx.store.getState().editorPath ?? "");
      if (!path) {
        ctx.notify(
          "/docgen",
          "No file. Open one in the editor or pass a path: /docgen <path>.",
          "warning",
        );
        return;
      }
      try {
        const { openDocGenModal } = await import("@/components/DocGenModal");
        openDocGenModal(path);
      } catch (e) {
        ctx.append(errorNote(`/docgen failed to mount: ${humanizeError(e)}`));
      }
    },
  },
];

/** Minimal subset of the Web Speech API. Kept local to avoid pulling DOM lib types. */
interface SpeechRecognitionLike {
  lang: string;
  interimResults: boolean;
  onresult: ((ev: SpeechRecognitionEventLike) => void) | null;
  onerror: ((ev: { error?: string }) => void) | null;
  start(): void;
}
interface SpeechRecognitionEventLike {
  results: ArrayLike<ArrayLike<{ transcript: string }>>;
}

/**
 * Compute history + attached-file char counts from the in-memory chat store.
 * Lets `/tokens` show useful numbers even on builds where the backend's
 * messages table isn't persisted yet.
 */
function localContextStats(messages: Message[]): {
  history_chars: number;
  history_message_count: number;
  attached_files_chars: number;
} {
  let history_chars = 0;
  for (const m of messages) {
    history_chars += m.content.length;
    if (m.reasoning) history_chars += m.reasoning.length;
  }
  // Attached files: scan the latest assistant message for `@file:` refs.
  // We can only measure their referenced *char count* (the path itself);
  // actual file sizes require a backend roundtrip the spec already covers.
  // Counting the raw path lengths keeps the column non-zero when @-mentions
  // exist and avoids lying about file contents we never read here.
  let attached_files_chars = 0;
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === "assistant") {
      const matches = messages[i].content.match(/@file:\S+/g) ?? [];
      for (const ref of matches) attached_files_chars += ref.length;
      break;
    }
  }
  return {
    history_chars,
    history_message_count: messages.length,
    attached_files_chars,
  };
}

/** Build a markdown table assistant message rendering the breakdown. */
function tokensBreakdownMessage(b: ContextBreakdown): Message {
  type Row = { label: string; chars: number };
  const rows: Row[] = [
    { label: "system prompt", chars: b.system_chars },
    { label: "CLAUDE.md", chars: b.claude_md_chars },
    { label: "rules", chars: b.rules_chars },
    { label: "repo map", chars: b.repo_map_chars },
    { label: `history (${b.history_message_count} msgs)`, chars: b.history_chars },
    { label: "attached files", chars: b.attached_files_chars },
  ];
  const totalChars = rows.reduce((acc, r) => acc + r.chars, 0);
  // Guard against div-by-zero so a fresh session renders 0% across the board
  // instead of NaN%.
  const pct = (n: number) =>
    totalChars === 0 ? 0 : Math.round((n / totalChars) * 100);
  const tokens = (chars: number) => Math.floor(chars / 4);
  const fmt = (n: number) => n.toLocaleString("en-US");

  const lines = [
    "| Component | Chars | ~Tokens | % |",
    "|---|---:|---:|---:|",
    ...rows.map(
      (r) => `| ${r.label} | ${fmt(r.chars)} | ${fmt(tokens(r.chars))} | ${pct(r.chars)}% |`,
    ),
    `| **total** | **${fmt(totalChars)}** | **${fmt(b.total_estimated_tokens)}** | **100%** |`,
  ];
  return {
    id: `tk-${crypto.randomUUID()}`,
    role: "assistant",
    agent: "/tokens",
    content: lines.join("\n"),
    tools: [],
    pending: false,
  };
}

/** All names+aliases, lowercase, mapped to their command. */
const INDEX: Map<string, SlashCommand> = (() => {
  const m = new Map<string, SlashCommand>();
  for (const c of COMMANDS) {
    m.set(c.name.toLowerCase(), c);
    for (const a of c.aliases ?? []) m.set(a.toLowerCase(), c);
  }
  return m;
})();

/**
 * Split a raw input like `"/compact   foo bar"` into `{ name, args }`.
 * Returns null when the input doesn't start with `/` or has no name word.
 */
export function parseInput(input: string): { name: string; args: string } | null {
  if (!input.startsWith("/")) return null;
  const match = input.slice(1).match(/^(\S+)\s*([\s\S]*)$/);
  return match ? { name: match[1], args: match[2] } : null;
}

/** Exact-match lookup on the leading word. Case-insensitive; null on miss. */
export function findCommand(input: string): SlashCommand | null {
  const parsed = parseInput(input);
  return parsed ? (INDEX.get(parsed.name.toLowerCase()) ?? null) : null;
}

/**
 * Prefix match for autocomplete. `prefix` may be `""`, `"c"`, `"/c"`, etc.
 * Each command appears at most once even if multiple aliases match.
 */
export function listMatching(prefix: string): SlashCommand[] {
  let p = prefix.trim();
  if (p.startsWith("/")) p = p.slice(1);
  // Only autocomplete on the first word — args mode kills the dropdown.
  p = p.split(/\s/, 1)[0]?.toLowerCase() ?? "";
  const seen = new Set<SlashCommand>();
  const out: SlashCommand[] = [];
  for (const [key, cmd] of INDEX) {
    if (key.startsWith(p) && !seen.has(cmd)) {
      seen.add(cmd);
      out.push(cmd);
    }
  }
  return out.sort((a, b) => a.name.localeCompare(b.name));
}

/** Build a SlashContext bound to the live store. */
export function makeContext(): SlashContext {
  return {
    store: { getState: useCortexStore.getState, setState: useCortexStore.setState },
    append: (msg) => useCortexStore.getState().appendMessage(msg),
    notify: (title, body, kind) => pushToast({ title, body, kind: kind ?? "info" }),
  };
}

// ---------- Custom slash commands ----------
//
// User-defined commands live at `~/.cortex/custom-slashes.yaml` and are
// loaded fire-and-forget at module init. The `INDEX` Map above is built
// once, so we expose `rebuildSlashIndex()` for the custom-slashes lib to
// call after mutating `COMMANDS` — without it, `findCommand` would never
// resolve a freshly-saved entry. For v1 this means the first lookup after
// app boot may miss if the disk read hasn't resolved yet; acceptable.

/** Clear + repopulate `INDEX` from the current `COMMANDS` array. Called by
 *  `pushCustomSlashes` after grafting user-defined entries onto the registry. */
export function rebuildSlashIndex(): void {
  INDEX.clear();
  for (const c of COMMANDS) {
    INDEX.set(c.name.toLowerCase(), c);
    for (const a of c.aliases ?? []) INDEX.set(a.toLowerCase(), c);
  }
}

COMMANDS.push({
  // `/slashes` (alias `/customslash`) opens the custom-slash builder modal.
  // Self-mounting portal — same pattern as `/vault`, `/audit`, etc.
  name: "slashes",
  aliases: ["customslash", "custom-slash"],
  description: "Open the custom slash command builder",
  run: async (_a, ctx) => {
    try {
      const { openCustomSlashBuilder } = await import(
        "@/components/CustomSlashBuilder"
      );
      openCustomSlashBuilder();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/slashes failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
// Re-seed INDEX so the newly-appended `/slashes` command resolves on first
// lookup. (The IIFE that originally built INDEX ran before this push.)
rebuildSlashIndex();

// Boot-time hydration: pull saved custom slashes off disk and graft them
// onto the registry. Fire-and-forget — failures degrade to "no customs"
// rather than blocking the chat input.
void (async () => {
  try {
    const { loadCustomSlashes, pushCustomSlashes } = await import(
      "@/lib/custom-slashes"
    );
    const loaded = await loadCustomSlashes();
    if (loaded.length > 0) {
      pushCustomSlashes(loaded);
    }
  } catch (err) {
    // No toast — this runs before the UI is mounted in some bundles.
    console.warn("custom slash hydration failed", err);
  }
})();

// `/preview` toggles the markdown preview pane on the currently-open
// editor file. Quietly no-ops (with a hint) when no file is open or
// the file isn't markdown, so chat users get feedback either way.
COMMANDS.push({
  name: "preview",
  aliases: ["mdpreview", "markdown-preview"],
  description: "Toggle the markdown preview pane in the editor",
  run: async (_a, ctx) => {
    const path = ctx.store.getState().editorPath;
    const { isMarkdownPath, toggleMarkdownPreview } = await import(
      "@/lib/markdown-preview"
    );
    if (!path) {
      ctx.notify("Preview", "Open a markdown file first.", "info");
      return;
    }
    if (!isMarkdownPath(path)) {
      ctx.notify(
        "Preview",
        "The current file isn't markdown — preview is only available for .md files.",
        "info",
      );
      return;
    }
    toggleMarkdownPreview();
  },
});
rebuildSlashIndex();

// ---------- AI changelog + project metrics ----------
//
// `/changelog [since]` opens the AI changelog generator modal seeded with
// the given range (default "2 weeks ago"). `/metrics` (alias `/stats`) jumps
// to the Project metrics activity-panel tab. Appended last so they
// participate in the final `INDEX` rebuild below.
COMMANDS.push({
  name: "changelog",
  description: "AI-generate a Keep-a-Changelog markdown from recent commits",
  usage: "[since]",
  run: async (args, ctx) => {
    try {
      const { openChangelogModal } = await import(
        "@/components/ChangelogModal"
      );
      openChangelogModal(args.trim() || undefined);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/changelog failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  name: "metrics",
  aliases: ["stats"],
  description: "Jump to the Project metrics panel (lines / langs / largest files)",
  run: (_a, ctx) => ctx.store.getState().setActivityTab("metrics"),
});
rebuildSlashIndex();

// ---------- AI project-doc generator ----------
//
// `/readme` and `/claude-md` open the project-level doc generator modal
// pre-selected to the matching doc type. The modal itself exposes a third
// "contributing" radio so users can switch without typing another slash.
// Mirrors the self-mounting portal pattern of `/changelog` / `/docgen` —
// no App.tsx wiring required.
COMMANDS.push({
  name: "readme",
  description: "AI-generate a polished README.md for the active project",
  run: async (_a, ctx) => {
    const project = ctx.store.getState().activeProject;
    if (!project) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: "No active project — pick one from the sidebar first.",
        tools: [],
      });
      return;
    }
    try {
      const { openProjectDocGenModal } = await import(
        "@/components/ProjectDocGenModal"
      );
      openProjectDocGenModal("readme");
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/readme failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  name: "claude-md",
  aliases: ["claudemd", "claude"],
  description: "AI-generate a CLAUDE.md (agent instructions) for the active project",
  run: async (_a, ctx) => {
    const project = ctx.store.getState().activeProject;
    if (!project) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: "No active project — pick one from the sidebar first.",
        tools: [],
      });
      return;
    }
    try {
      const { openProjectDocGenModal } = await import(
        "@/components/ProjectDocGenModal"
      );
      openProjectDocGenModal("claude-md");
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/claude-md failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Workspace presets ----------
//
// `/preset` (no arg) and `/layout` open the WorkspacePresetsModal portal so
// the user can save / restore named bundles of UI state. With an argument
// (`/preset deep-work`) we skip the modal and apply the preset directly,
// surfacing the result as a toast.
COMMANDS.push({
  name: "preset",
  description: "Save or restore a named workspace layout preset",
  usage: "[name]",
  run: async (args, ctx) => {
    const name = args.trim();
    if (!name) {
      try {
        const { openWorkspacePresetsModal } = await import(
          "@/components/WorkspacePresetsModal"
        );
        openWorkspacePresetsModal();
      } catch (e) {
        ctx.append({
          id: `e-${crypto.randomUUID()}`,
          role: "error",
          content: `/preset failed to mount: ${humanizeError(e)}`,
          tools: [],
        });
      }
      return;
    }
    try {
      const { applyPreset } = await import("@/lib/workspace-presets");
      const report = await applyPreset(name);
      if (!report) {
        ctx.notify("/preset", `No preset named '${name}'.`, "warning");
        return;
      }
      const appliedSummary =
        report.applied.length > 0 ? `applied: ${report.applied.join(", ")}` : "nothing applied";
      const skippedSummary =
        report.skipped.length > 0 ? ` · skipped: ${report.skipped.join(", ")}` : "";
      ctx.notify(
        `Preset '${name}' applied`,
        `${appliedSummary}${skippedSummary}`,
        report.applied.length > 0 ? "success" : "warning",
      );
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/preset failed: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  name: "layout",
  description: "Open the workspace presets modal (alias of /preset)",
  run: async (_a, ctx) => {
    try {
      const { openWorkspacePresetsModal } = await import(
        "@/components/WorkspacePresetsModal"
      );
      openWorkspacePresetsModal();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/layout failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Dependency import graph ----------
//
// `/deps` opens the SVG force-directed dependency import graph for the
// active project. The panel itself walks the project on mount via the
// `build_dep_graph` Tauri command, so no args are needed; future flags
// (e.g. `--lang=ts` to pre-filter) would attach here without touching
// ActivityPanel.
COMMANDS.push({
  name: "deps",
  aliases: ["depgraph", "dep-graph"],
  description: "Open the dependency import graph for the active project",
  run: (_a, ctx) => ctx.store.getState().setActivityTab("dep-graph"),
});
rebuildSlashIndex();

// ---------- Unified notification center ----------
//
// `/notifs` (alias `/notifications`) opens the aggregated inbox modal that
// pulls from crashes/issues/audit + monitor/config/repo event streams. Same
// self-mounting portal pattern as `/audit`, `/crashes`, etc. — App.tsx is
// intentionally untouched. The StatusBar 🔔 badge summons the same modal.
COMMANDS.push({
  name: "notifs",
  aliases: ["notifications"],
  description: "Open the unified notification center (crashes + issues + audit + monitors + repo)",
  run: async (_a, ctx) => {
    try {
      const { openNotificationCenter } = await import(
        "@/lib/notification-center"
      );
      await openNotificationCenter();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/notifs failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- AI explain mode ----------
//
// `/explain [path]` — beginner-friendly walk-through of the file (defaults to
// the currently-open editor path, whole file). `/why [start:end]` — same
// modal but pre-seeded to a specific line range of the editor path so users
// can ask "why does this block exist?" without typing a path. Both open the
// self-mounting `ExplainModal` portal so App.tsx wiring stays untouched.
COMMANDS.push({
  name: "explain",
  description: "AI-explain a file (or line range) for a beginner reader",
  usage: "[path]",
  run: async (args, ctx) => {
    const explicit = args.trim();
    const path = explicit || (ctx.store.getState().editorPath ?? "");
    if (!path) {
      ctx.notify(
        "/explain",
        "No file. Open one in the editor or pass a path: /explain <path>.",
        "warning",
      );
      return;
    }
    try {
      const { openExplainModal } = await import("@/components/ExplainModal");
      openExplainModal(path);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/explain failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  // `/why [start:end]` — accepts `12:40`, `12-40`, or a single line `12`.
  // No-arg form falls through to the whole editor path so the modal still
  // mounts (with empty range inputs the user can dial in interactively).
  name: "why",
  description: "AI-explain a specific line range of the currently-open file",
  usage: "[start:end]",
  run: async (args, ctx) => {
    const path = ctx.store.getState().editorPath ?? "";
    if (!path) {
      ctx.notify(
        "/why",
        "No file is open in the editor — `/explain <path>` instead.",
        "warning",
      );
      return;
    }
    const raw = args.trim();
    let lineStart: number | null = null;
    let lineEnd: number | null = null;
    if (raw) {
      const match = raw.match(/^(\d+)(?:\s*[:\-]\s*(\d+))?$/);
      if (!match) {
        ctx.notify("/why", "Usage: /why [start:end]", "warning");
        return;
      }
      lineStart = Number.parseInt(match[1], 10);
      lineEnd = match[2] ? Number.parseInt(match[2], 10) : lineStart;
    }
    try {
      const { openExplainModal } = await import("@/components/ExplainModal");
      openExplainModal(path, lineStart, lineEnd);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/why failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Bookmarks / favorites ----------
//
// `/bookmark <label>` (alias `/star`) quick-adds a "note" kind bookmark with
// the label as both label + target — handy for jotting down a thought from
// the chat composer without leaving the keyboard. `/bookmarks` (alias
// `/stars`) jumps to the dedicated activity-panel tab where users can pin
// memory entries, files, traces, sessions, and URLs.
COMMANDS.push({
  name: "bookmark",
  aliases: ["star"],
  description: "Quick-add a note-kind bookmark from the chat composer",
  usage: "<label>",
  run: async (args, ctx) => {
    const label = args.trim();
    if (!label) {
      ctx.notify("/bookmark", "Usage: /bookmark <label>", "warning");
      return;
    }
    try {
      const { addBookmark } = await import("@/lib/bookmarks");
      const saved = await addBookmark({
        kind: "note",
        label,
        target: label,
        tags: [],
        note: null,
      });
      if (!saved) {
        ctx.append({
          id: `e-${crypto.randomUUID()}`,
          role: "error",
          content: `/bookmark failed: backend rejected bookmark.`,
          tools: [],
        });
        return;
      }
      ctx.notify("Bookmarked", `⭐ ${saved.label}`, "success");
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/bookmark failed: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  name: "bookmarks",
  aliases: ["stars"],
  description: "Open the Bookmarks panel (pinned files, traces, sessions, URLs)",
  run: (_a, ctx) => ctx.store.getState().setActivityTab("bookmarks"),
});
rebuildSlashIndex();

// ---------- AI test generator ----------
//
// `/gentest [path] [:: function]` (alias `/testgen`) generates unit tests
// for the target file or a single function inside it. Defaults `path` to the
// currently-open editor path so the no-arg form is a one-keystroke ask. The
// `::function` suffix is optional and scopes the tests to that one symbol.
// Opens the self-mounting `TestGenModal` portal — same pattern as `/gentest`'s
// sibling `/docgen` / `/refactor` / `/explain`.
COMMANDS.push({
  name: "gentest",
  aliases: ["testgen"],
  description: "AI-generate unit tests for a file or a specific function",
  usage: "[path] [:: function]",
  run: async (args, ctx) => {
    const raw = args.trim();
    let path = "";
    let fn: string | undefined;
    if (raw.includes("::")) {
      const [p, ...rest] = raw.split("::");
      path = (p ?? "").trim();
      const focus = rest.join("::").trim();
      if (focus) fn = focus;
    } else {
      path = raw;
    }
    if (!path) {
      path = ctx.store.getState().editorPath ?? "";
    }
    if (!path) {
      ctx.notify(
        "/gentest",
        "No file. Open one in the editor or pass a path: /gentest <path>.",
        "warning",
      );
      return;
    }
    try {
      const { openTestGenModal } = await import("@/components/TestGenModal");
      openTestGenModal(path, fn ?? null);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/gentest failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- AI debugger ----------
//
// `/fix` (alias `/debug`) opens the DebuggerModal — a portal modal that asks
// the gateway to diagnose an error and propose a unified-diff patch. The default
// source is `recent_crash`; the modal lets the user switch sources or paste
// a manual error. When a `chat_error` role message exists in the current
// session we forward it as `chat_error` source so the user doesn't have to
// re-paste it.
COMMANDS.push({
  name: "fix",
  aliases: ["debug"],
  description: "AI-debug the most recent error and propose a unified-diff patch",
  usage: "[crash|issue|test|chat|manual]",
  run: async (args, ctx) => {
    const raw = args.trim().toLowerCase();
    type DebugSource =
      | "recent_crash"
      | "recent_issue"
      | "last_test_failure"
      | "chat_error"
      | "manual";
    const sourceMap: Record<string, DebugSource> = {
      crash: "recent_crash",
      crashes: "recent_crash",
      issue: "recent_issue",
      issues: "recent_issue",
      test: "last_test_failure",
      tests: "last_test_failure",
      chat: "chat_error",
      manual: "manual",
      paste: "manual",
    };
    let initialSource: DebugSource = sourceMap[raw] ?? "recent_crash";
    let errorText: string | undefined;
    let errorStack: string | undefined;

    // Auto-promote to `chat_error` when an error-role message exists in the
    // current scroll and the user didn't pick a specific source. Saves a
    // round-trip for the "I just saw a red bubble — fix it" case.
    if (!raw || raw === "crash" || raw === "chat") {
      const messages = ctx.store.getState().messages;
      const lastErr = [...messages].reverse().find((m) => m.role === "error");
      if (lastErr) {
        if (!raw || raw === "chat") {
          initialSource = "chat_error";
          errorText = lastErr.content;
        }
      }
    }
    try {
      const { openDebuggerModal } = await import("@/components/DebuggerModal");
      openDebuggerModal(initialSource, { errorText, errorStack });
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/fix failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Natural-language slash router ----------
//
// `/ask <query>` is the "I don't remember the right slash" escape hatch. It
// ships the live `COMMANDS` array (names + descriptions + aliases) to the gateway,
// which picks the best match and any args. High-confidence matches run
// immediately; ambiguous matches surface a confirm toast + system note; no
// match leaves a system note explaining why.
//
// Implementation lives in `@/lib/ask-router` so the dispatch logic is
// testable on its own and reusable from a future omnibar surface.
COMMANDS.push({
  name: "ask",
  aliases: ["?"],
  description: "Ask in natural language — routes to the best matching slash",
  usage: "<natural language query>",
  run: async (args, ctx) => {
    const { runAsk } = await import("@/lib/ask-router");
    await runAsk(args, ctx);
  },
});
rebuildSlashIndex();

// ---------- Category routing (palette grouping) ----------
//
// `categorize(name)` resolves a slash command's UI bucket by exact-name
// match. Keeps the mapping declarative in one place so CommandPalette's
// render layer is the only consumer; existing COMMANDS entries are
// untouched. Lookup is case-insensitive on the canonical `name` only —
// aliases route to their primary command via INDEX before reaching here.
const CATEGORY_MAP: Record<string, string> = (() => {
  const m: Record<string, string> = {};
  const groups: Record<string, string[]> = {
    Git: ["commit", "push", "ship", "stage", "conflict", "audit-deps", "commit-msg"],
    AI: [
      "ask", "explain", "why", "fix", "debug", "refactor", "docgen", "readme",
      "claude-md", "summary", "gentest", "testgen", "changelog", "duck", "journal",
    ],
    Memory: [
      "memory", "memstats", "memnew", "new-memory", "brain-save", "dedupe",
      "sync", "snapshot", "snap",
    ],
    Project: [
      "projects", "search", "find", "open", "p", "metrics", "stats", "deps",
      "graph", "knowledge",
    ],
    Workflow: ["workflow", "wf", "preset", "layout", "team", "teams", "focus", "trust"],
    Cortex: [
      "clear", "new", "reset", "settings", "shortcuts", "help", "tour",
      "export", "save", "stop", "tokens", "usage", "compact", "note",
      "cookbook", "routines", "eval", "setup", "research", "deep-research",
    ],
    Debug: ["test", "audit", "crashes", "crash", "obs", "observability", "vault", "webhook"],
  };
  for (const [cat, names] of Object.entries(groups)) {
    for (const n of names) m[n.toLowerCase()] = cat;
  }
  return m;
})();

/**
 * Map a slash command name → its palette category. Falls back to "Other"
 * when the name isn't in the explicit routing table. Pure / synchronous so
 * CommandPalette can call it inside a `useMemo` without async juggling.
 */
export function categorize(name: string): string {
  return CATEGORY_MAP[name.toLowerCase()] ?? "Other";
}

/** Stable display order for category headers in the palette. "Other" is
 *  always last so unmatched commands sink to the bottom of the list. */
export const CATEGORY_ORDER: readonly string[] = [
  "Git",
  "AI",
  "Memory",
  "Project",
  "Workflow",
  "Cortex",
  "Debug",
  "Other",
];

// ---------- AI merge conflict resolver ----------
//
// `/conflict` (alias `/resolve`) opens the ConflictResolverModal — a portal
// modal that scans the active project for files with unresolved merge
// markers, asks the gateway for a clean resolution per file, and lets the user
// accept/skip + stage on confirm. Self-mounting portal, same pattern as
// `/refactor` / `/docgen` so App.tsx stays untouched.
COMMANDS.push({
  name: "conflict",
  aliases: ["resolve"],
  description: "AI-resolve unresolved merge conflicts in the active project",
  run: async (_a, ctx) => {
    const project = ctx.store.getState().activeProject;
    if (!project) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: "No active project — pick one from the sidebar first.",
        tools: [],
      });
      return;
    }
    try {
      const { openConflictResolverModal } = await import(
        "@/components/ConflictResolverModal"
      );
      openConflictResolverModal(project.root);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/conflict failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});

// ---------- Dependency vulnerability audit ----------
//
// `/audit-deps` (alias `/vuln`) opens the DepAuditModal — a portal modal
// that runs the project's ecosystem-specific audit tool (npm/cargo/pip),
// parses the JSON into a normalised list, and shows severity-tinted rows
// with explain / open / advisory actions. Self-mounting portal, same
// pattern as `/conflict` above. The standalone `/audit` slash already
// exists for the audit log viewer — kept separate to avoid colliding
// with that older binding.
COMMANDS.push({
  name: "audit-deps",
  aliases: ["vuln"],
  description: "Scan dependencies for known vulnerabilities (npm/cargo/pip)",
  run: async (_a, ctx) => {
    const project = ctx.store.getState().activeProject;
    if (!project) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: "No active project — pick one from the sidebar first.",
        tools: [],
      });
      return;
    }
    try {
      const { openDepAuditModal } = await import(
        "@/components/DepAuditModal"
      );
      openDepAuditModal(project.root);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/audit-deps failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});

// ---------- Rubber duck Socratic chat ----------
//
// `/duck <topic>` opens the DuckChat portal — a Socratic AI partner that
// asks clarifying questions instead of giving direct answers. Transcript
// state lives in the component; backend is stateless and replays the full
// thread on each call. Self-mounting portal, same pattern as `/explain` /
// `/conflict`. Optional `Save transcript to brain` writes the dialog out to
// `~/Documents/Cortex Brain/duck/<date>-<slug>.md`.
COMMANDS.push({
  name: "duck",
  description: "Open a Socratic rubber-duck chat (asks questions, never answers)",
  usage: "[topic]",
  run: async (args, ctx) => {
    try {
      const { openDuckChat } = await import("@/components/DuckChat");
      openDuckChat(args.trim());
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/duck failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});

// ---------- Daily journal ----------
//
// `/journal [YYYY-MM-DD]` opens the DailyJournalModal — collates sessions,
// commits, memory updates, snapshots, and PRP activity for the given day
// (defaults to today), asks the gateway for a markdown summary with the four
// required headers, and offers a "Save to Cortex Brain" button that writes
// the file under `~/Documents/Cortex Brain/journal/<date>.md`. Self-mounting
// portal, same pattern as `/duck` above.
COMMANDS.push({
  name: "journal",
  description: "Open the daily activity journal (defaults to today)",
  usage: "[YYYY-MM-DD]",
  run: async (args, ctx) => {
    try {
      const { openDailyJournalModal } = await import(
        "@/components/DailyJournalModal"
      );
      openDailyJournalModal(args.trim() || undefined);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/journal failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Git stash manager ----------
//
// `/stash` opens the StashManagerModal — a portal modal that lists the
// project's stashes (ref, subject, age, file-count badge) with per-row
// Apply / Pop / Drop / Diff actions and a "Stash current changes" header
// form (optional message, include-untracked toggle). Self-mounting portal,
// same pattern as `/journal` / `/duck` above so App.tsx stays untouched.
COMMANDS.push({
  name: "stash",
  description: "Open the git stash manager for the active project",
  run: async (_a, ctx) => {
    const project = ctx.store.getState().activeProject;
    if (!project) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: "No active project — pick one from the sidebar first.",
        tools: [],
      });
      return;
    }
    try {
      const { openStashManagerModal } = await import(
        "@/components/StashManagerModal"
      );
      openStashManagerModal();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/stash failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Cortex Brain table of contents ----------
//
// `/toc` (alias `/brain-toc`) opens the BrainTocModal — a portal modal that
// walks every memory source (Claude project memory, runbooks, Obsidian,
// project / global instructions) and renders each markdown file's heading
// hierarchy. Clicking a heading dispatches `cortex:editor-open` with the
// path so the editor pane opens the file. Self-mounting portal, same pattern
// as `/stash` / `/journal` above so App.tsx stays untouched.
COMMANDS.push({
  name: "toc",
  aliases: ["brain-toc"],
  description: "Open the Cortex Brain table of contents (every memory source)",
  run: async (_a, ctx) => {
    try {
      const { openBrainTocModal } = await import("@/components/BrainTocModal");
      openBrainTocModal();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/toc failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Gitea backup auto-mirror ----------
//
// `/gitea-backup` opens the GiteaBackupPanel — settings form (base URL,
// token, owner, repo, enabled toggle), last-backup status, "Backup now"
// trigger, and Open-repo shortcut. The Tauri backend runs the same backup
// every 6 hours when the enabled toggle is on. `/backup-now` skips the
// panel and triggers an immediate backup using whatever is currently
// persisted in `~/.cortex/gitea-config.json`. Self-mounting portal, same
// pattern as `/toc` above.
COMMANDS.push({
  name: "gitea-backup",
  description: "Configure + trigger the Gitea backup auto-mirror",
  run: async (_a, ctx) => {
    try {
      const { openGiteaBackupPanel } = await import(
        "@/components/GiteaBackupPanel"
      );
      openGiteaBackupPanel();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/gitea-backup failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  name: "backup-now",
  description: "Trigger an immediate Gitea backup (requires prior config)",
  run: async (_a, ctx) => {
    try {
      const { runBackupNow } = await import("@/lib/gitea-backup");
      const { pushToast } = await import("@/lib/toast");
      const report = await runBackupNow();
      const ok = report.errors.length === 0;
      pushToast({
        title: ok ? "Backup complete" : "Backup finished with errors",
        body: `${report.files_added}+ ${report.files_changed}~ ${report.files_deleted}- · ${report.commits_made} commit(s)`,
        kind: ok ? "success" : "error",
      });
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/backup-now failed: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Wave-4: Gather/Agent mode, pinned notes, recipe gallery ----------
//
// `/gather` (alias `/agent-mode`) — flip between the read-only "gather"
// trust profile and the user's saved agent profile. Persists in
// localStorage; writes through to the trust-matrix file on disk so the
// approval pipeline picks the change up transparently.
//
// `/pin <path>` — read the given memory entry and add it to the pinned-
// notes rail. Content is capped at 16 KiB by the storage layer.
//
// `/recipes` — open the recipe gallery modal (self-mounting portal,
// same pattern as `/vault` / `/audit`).
COMMANDS.push({
  name: "gather",
  aliases: ["agent-mode"],
  description: "Toggle Gather (read-only) vs Agent (full write+exec) mode",
  run: async (_a, ctx) => {
    try {
      const { toggleMode } = await import("@/lib/gather-mode");
      await toggleMode();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/gather failed: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  name: "pin",
  description: "Pin a memory file (or label) to the chat composer rail",
  usage: "<path|label>",
  run: async (args, ctx) => {
    const raw = args.trim();
    if (!raw) {
      ctx.notify("/pin", "Usage: /pin <path>", "warning");
      return;
    }
    try {
      const { getMemoryEntry } = await import("@/lib/memory");
      const { addPinnedNote } = await import("@/lib/pinned-notes");
      // Try as a memory path first; if that throws we fall back to
      // pinning the raw string as a label-only chip so users get an
      // affordance even when the path doesn't resolve.
      try {
        const entry = await getMemoryEntry(raw);
        const label = entry.title || raw.split(/[/\\]/).pop() || raw;
        await addPinnedNote({
          label,
          content: entry.body,
          source_path: entry.path,
        });
        ctx.notify("Pinned", label, "success");
        return;
      } catch {
        await addPinnedNote({ label: raw, content: raw, source_path: null });
        ctx.notify("Pinned", raw, "success");
      }
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/pin failed: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
COMMANDS.push({
  name: "recipes",
  description: "Open the recipe gallery (Goose-style YAML workflows)",
  run: async (_a, ctx) => {
    try {
      const { openRecipeGallery } = await import("@/components/RecipeGallery");
      openRecipeGallery();
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/recipes failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Model arena ----------
//
// `/arena <prompt>` (alias `/compare`) opens the Model Arena tab, optionally
// pre-filling the prompt textarea via a module-scoped slot inside ArenaPane.
// No-arg form just switches tabs and lets the user pick models + type a
// prompt inline. Mirrors the `setSearchPreload` pattern used by `/search`.
COMMANDS.push({
  name: "arena",
  aliases: ["compare"],
  description: "Open the Model Arena (side-by-side A/B compare across 2-4 models)",
  usage: "[prompt]",
  run: async (args, ctx) => {
    const prompt = args.trim();
    if (prompt) {
      try {
        const { setArenaPreload } = await import("@/components/ArenaPane");
        setArenaPreload(prompt);
      } catch {
        /* component not mounted yet — preload will be picked up on next mount */
      }
    }
    ctx.store.getState().setActivityTab("arena");
  },
});
rebuildSlashIndex();

// ---------- Batch runner ----------
//
// `/batch <a | b | c> :: <prompt with {{item}}>` opens the CrewAI-style
// `kickoff_for_each` modal pre-filled with items + template. Either side of
// the `::` can be empty — the modal will let the user fill in the missing
// piece. No-arg form opens an empty modal.
COMMANDS.push({
  name: "batch",
  aliases: ["kickoff", "foreach"],
  description: "Run one prompt across N items in parallel (CrewAI kickoff_for_each)",
  usage: "[items pipe-separated] :: [prompt with {{item}}]",
  run: async (args, ctx) => {
    try {
      const { openBatchRunnerModal } = await import("@/components/BatchRunnerModal");
      const { parseBatchSlash } = await import("@/lib/batch-runner");
      const parsed = parseBatchSlash(args);
      if (parsed) {
        openBatchRunnerModal({ items: parsed.items, prompt: parsed.promptTemplate });
      } else {
        openBatchRunnerModal();
      }
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/batch failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Multi-agent Channels ----------
//
// `/channel <name>` jumps to the Channels activity tab pre-selecting the named
// channel; if no channel with that name exists yet we create a user-only stub
// so the panel lands on something interactive. `/channels` (alias) with no arg
// just opens the tab.
COMMANDS.push({
  name: "channel",
  aliases: ["channels"],
  description: "Open multi-agent Channels (optionally pre-select / create by name)",
  usage: "[name]",
  run: async (args, ctx) => {
    const name = args.trim();
    try {
      if (name) {
        const { openOrCreateChannelByName } = await import("@/lib/channels");
        const { setChannelsPreselect } = await import("@/components/ChannelsPanel");
        const channel = await openOrCreateChannelByName(name);
        setChannelsPreselect(channel.id);
      }
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/channel failed: ${humanizeError(e)}`,
        tools: [],
      });
    }
    ctx.store.getState().setActivityTab("channels");
  },
});
rebuildSlashIndex();

COMMANDS.push({
  // CrewAI-style manager process. `/manager <goal>` (alias `/auto`) opens a
  // portal modal that asks the manager LLM to decompose the goal into role-
  // tagged subtasks, then runs each step sequentially with auto-validation.
  // Same self-mounting pattern as `/batch`, `/refactor`, etc. — App.tsx stays
  // untouched.
  name: "manager",
  aliases: ["auto"],
  description:
    "Auto-decompose a goal into role-tagged subtasks, then run + validate each step",
  usage: "<goal>",
  run: async (args, ctx) => {
    const goal = args.trim();
    try {
      const { openManagerProcessModal } = await import(
        "@/components/ManagerProcessModal"
      );
      openManagerProcessModal(goal || undefined);
    } catch (e) {
      ctx.append({
        id: `e-${crypto.randomUUID()}`,
        role: "error",
        content: `/manager failed to mount: ${humanizeError(e)}`,
        tools: [],
      });
    }
  },
});
rebuildSlashIndex();

// ---------- Zed-style multibuffer ----------
//
// `/multi` (alias `/mb`) jumps to the multibuffer activity tab. The panel
// itself owns the excerpt list + per-excerpt CodeMirror editors; search,
// refactor, and hunk-review surfaces can route into it later by
// dispatching `cortex:multibuffer-open` or calling `addExcerpt(...)`
// from `@/lib/multibuffer`.
COMMANDS.push({
  name: "multi",
  aliases: ["mb"],
  description: "Jump to the Multibuffer (Zed-style stitched excerpts)",
  run: (_a, ctx) => ctx.store.getState().setActivityTab("multibuffer"),
});
rebuildSlashIndex();

// ---------- Smart-context auto-picker ----------
//
// `/suggest-context` (alias `/ctx`) asks the gateway which `@`-tokens the user
// should attach to their current draft. The actual fetch + UI lives in
// `ChatPane`; the slash just dispatches a window event so the pane can
// trigger the same flow as the "🎯 Suggest context" button.
COMMANDS.push({
  name: "suggest-context",
  aliases: ["ctx"],
  description:
    "Suggest @-tokens (files / memory / diff / problems) to attach to the current draft",
  run: (_a, _ctx) => {
    window.dispatchEvent(new CustomEvent("cortex:suggest-context"));
  },
});

// `/save-frag <name>` — persist the current composer draft as a reusable
// fragment under `~/.cortex/fragments/<name>.md`. Lets the user capture
// any tested prompt template inline without leaving the app.
COMMANDS.push({
  name: "save-frag",
  aliases: ["savefrag"],
  description: "Save current draft as a reusable fragment (@frag:<name>)",
  usage: "<name>",
  run: async (args, ctx) => {
    const name = (args ?? "").trim();
    if (!name) {
      ctx.append(errorNote("/save-frag needs a name. e.g. `/save-frag arch`"));
      return;
    }
    // Read the live textarea value — `input` is local to ChatPane, not in
    // the Zustand store, so the store doesn't reflect drafts.
    const body = (document.querySelector<HTMLTextAreaElement>(".chat-input textarea")?.value ?? "").trim();
    if (!body) {
      ctx.append(errorNote("/save-frag: composer is empty."));
      return;
    }
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("save_fragment", { name, body });
      ctx.notify("Fragment saved", `Use @frag:${name} to inline it later.`, "success");
    } catch (e) {
      ctx.append(errorNote(`/save-frag failed: ${humanizeError(e)}`));
    }
  },
});

// `/fragments` — list saved fragments as a system message so the user
// can see what's available before typing `@frag:`. Reads from the same
// `list_fragments` command the picker uses.
COMMANDS.push({
  name: "fragments",
  aliases: ["frags", "frag-list"],
  description: "List saved fragments (~/.cortex/fragments/*.md)",
  run: async (_a, ctx) => {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const names = await invoke<string[]>("list_fragments");
      if (names.length === 0) {
        ctx.append(systemNote("No fragments yet. Save one with `/save-frag <name>` while a draft is open."));
        return;
      }
      const body = names.map((n) => `- \`@frag:${n}\``).join("\n");
      ctx.append(systemNote(`### Fragments (${names.length})\n\n${body}\n\nInline any of them with the listed @-token.`));
    } catch (e) {
      ctx.append(errorNote(`/fragments failed: ${humanizeError(e)}`));
    }
  },
});

// `/brain` / `/diff` / `/recent` — splice the magic tokens into the
// composer at the cursor position. Same handler the quick-attach toolbar
// uses, just keyboard-accessible.
// `/grep <pattern>` — splice `@grep:<pattern>` at the cursor for one-token
// project search. Backend resolves at chat-send time.
COMMANDS.push({
  name: "grep",
  description: "Search project for <pattern> via @grep token",
  usage: "<pattern>",
  run: (a, _ctx) => {
    const p = (a ?? "").trim();
    if (!p) return;
    window.dispatchEvent(
      new CustomEvent("cortex:composer-insert", { detail: { value: `@grep:${p}` } }),
    );
  },
});

// Wave 123 — `/summarize` invokes the existing summarize_session backend
// (gateway-driven) on the current session and saves to ~/Documents/Cortex
// Brain/sessions/<id>-summary.md when `/summarize save` is used. Append the
// headline + body inline so the user can scan without context-switching.
COMMANDS.push({
  name: "summarize",
  description: "AI summary of this session (headline + body); add 'save' to also write to brain",
  usage: "[save]",
  run: async (a, ctx) => {
    const state = ctx.store.getState();
    const sessionId: string | undefined = state.sessionId;
    if (!sessionId) {
      ctx.notify("/summarize skipped", "No active session id.", "warning");
      return;
    }
    const save = (a ?? "").trim().toLowerCase() === "save";
    // Wave 231 — short-circuit when the session has no messages so we
    // don't pay a gateway round-trip for an obvious "nothing to do".
    const msgCount = state.messages?.length ?? 0;
    if (msgCount === 0) {
      ctx.notify("/summarize skipped", "Session has no messages.", "warning");
      return;
    }
    ctx.notify("/summarize started", `Summarizing ${msgCount} message(s)…`, "info");
    try {
      const { summarizeSession } = await import("@/lib/session-summary");
      const summary = await summarizeSession(sessionId, save);
      const where = summary.saved_path
        ? `\n\n_saved → ${summary.saved_path}_`
        : "";
      ctx.append(
        systemNote(
          `**${summary.headline}**\n\n${summary.body}${where}`,
        ),
      );
      ctx.notify(
        save ? "/summarize saved" : "/summarize done",
        summary.headline,
        "success",
      );
    } catch (e) {
      ctx.append(errorNote(`/summarize failed: ${humanizeError(e)}`));
      ctx.notify("/summarize failed", humanizeError(e), "error");
    }
  },
});

// Wave 244 — `/clear-cache` drops all entries from the repo_map cache.
// Useful when you've just renamed/added files and want @repomap to pick
// up the changes immediately instead of waiting for the 10s TTL.
COMMANDS.push({
  name: "clear-cache",
  description: "Drop all entries from the repo_map cache",
  run: async (_a, ctx) => {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const n = await invoke<number>("repo_map_cache_clear");
      ctx.notify("/clear-cache", `Cleared ${n} entries`, "success");
    } catch (e) {
      ctx.notify("/clear-cache failed", humanizeError(e), "error");
    }
  },
});

// Wave 232 — `/memory-paths` lists the memory source roots the brain
// scans. Useful diagnostic for "is my .md actually in the brain".
COMMANDS.push({
  name: "memory-paths",
  description: "Show the memory source paths the brain scans",
  run: async (_a, ctx) => {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const dirs = await invoke<Array<{ label: string; root: string; kind: string }>>(
        "list_memory_sources",
      );
      if (dirs.length === 0) {
        ctx.notify("/memory-paths", "No memory sources discovered.", "info");
        return;
      }
      // Wave 292 — sort by kind weight (highest first) so the most
      // influential sources surface at the top. Mirrors the backend's
      // source_kind_weight ordering: claude_project_memory > project >
      // obsidian > runbooks > global.
      const kindOrder: Record<string, number> = {
        claude_project_memory: 5,
        project_instructions: 4,
        obsidian: 3,
        runbooks: 2,
        global_instructions: 1,
      };
      const sorted = [...dirs].sort(
        (a, b) => (kindOrder[b.kind] ?? 0) - (kindOrder[a.kind] ?? 0),
      );
      const lines = sorted.map((d) => `${d.label}: ${d.root} (${d.kind})`).join("\n");
      ctx.append(systemNote(`**Brain memory sources** (highest weight first)\n\n\`\`\`\n${lines}\n\`\`\``));
    } catch (e) {
      ctx.notify("/memory-paths failed", humanizeError(e), "error");
    }
  },
});

// Wave 229 — `/version` shows the Cortex CLI/app version + tauri runtime.
COMMANDS.push({
  name: "version",
  description: "Show Cortex version",
  run: async (_a, ctx) => {
    try {
      const { getVersion } = await import("@tauri-apps/api/app");
      const v = await getVersion();
      ctx.notify("Cortex version", `v${v}`, "info");
    } catch (e) {
      ctx.notify("/version failed", humanizeError(e), "error");
    }
  },
});

// Wave 226 — `/cache-stats` shows the repo_map cache size. Diagnostic.
COMMANDS.push({
  name: "cache-stats",
  description: "Show repo_map cache stats (entries / TTL)",
  run: async (_a, ctx) => {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const n = await invoke<number>("repo_map_cache_stats");
      ctx.notify(
        "/cache-stats",
        `${n} entries in repo_map cache (10s TTL; max 16 entries with LRU eviction)`,
        "info",
      );
    } catch (e) {
      ctx.notify("/cache-stats failed", humanizeError(e), "error");
    }
  },
});

// Wave 206 — `/diff-stats` shows git diff --stat output for the active
// project. Cheaper view than @diff for "what's changed this session".
COMMANDS.push({
  name: "diff-stats",
  description: "Show git diff --stat (default vs HEAD; pass a ref to compare vs that)",
  usage: "[ref]",
  run: async (a, ctx) => {
    const state = ctx.store.getState();
    const root: string | undefined = state.activeProject?.root;
    if (!root) {
      ctx.notify("/diff-stats skipped", "No active project.", "warning");
      return;
    }
    try {
      const { Command } = await import("@tauri-apps/plugin-shell");
      // Wave 263 — optional ref argument. Defaults to HEAD; users can
      // pass `main` or any other ref to compare against. Light input
      // validation: only allow ref chars [\w/.-] so we don't feed a
      // shell-injection target to git.
      const ref = (a ?? "").trim() || "HEAD";
      if (!/^[A-Za-z0-9_./-]{1,128}$/.test(ref)) {
        ctx.notify("/diff-stats skipped", `Bad ref "${ref}".`, "warning");
        return;
      }
      const out = await Command.create("git", ["-C", root, "diff", "--stat", ref]).execute();
      const txt = (out.stdout ?? "").trim();
      if (!txt) {
        ctx.notify("/diff-stats", `No diff vs ${ref}.`, "info");
        return;
      }
      ctx.append(systemNote(`**git diff --stat ${ref}**\n\n\`\`\`\n${txt}\n\`\`\``));
    } catch (e) {
      ctx.append(errorNote(`/diff-stats failed: ${humanizeError(e)}`));
    }
  },
});

// Wave 193 — `/repomap-top` displays the top 10 files by PageRank score
// from the active project. Quick way to see what's "central" without
// pasting the full @repomap into the chat.
COMMANDS.push({
  name: "repomap-top",
  description: "Show the top N files by PageRank (default 10, max 50)",
  usage: "[N]",
  run: async (a, ctx) => {
    const state = ctx.store.getState();
    const root: string | undefined = state.activeProject?.root;
    if (!root) {
      ctx.notify("/repomap-top skipped", "No active project.", "warning");
      return;
    }
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const map = await invoke<{ files: Array<{ path: string; pagerank: number; symbols: Array<unknown> }> }>(
        "compute_repo_map_command",
        { projectRoot: root, maxFiles: 500 },
      );
      // Wave 202 — accept optional N (clamp 1..=50; default 10).
      const argN = parseInt((a ?? "").trim(), 10);
      const n = Number.isFinite(argN) && argN > 0 ? Math.min(50, argN) : 10;
      const top = map.files.slice(0, n);
      if (top.length === 0) {
        // Wave 269 — point users at the cache invalidator since a freshly
        // added file might not be picked up if the cache still has a
        // pre-add result.
        ctx.notify(
          "/repomap-top",
          "Empty repo map. Try /clear-cache if you just added files.",
          "info",
        );
        return;
      }
      const lines = top
        .map((f, i) => `${i + 1}. ${f.path} ★${f.pagerank.toFixed(2)} (${f.symbols.length} symbols)`)
        .join("\n");
      ctx.append(systemNote(`**Top ${n} by PageRank**\n\n\`\`\`\n${lines}\n\`\`\``));
    } catch (e) {
      ctx.append(errorNote(`/repomap-top failed: ${humanizeError(e)}`));
    }
  },
});

// Wave 164 — `/extracted <text>` shows the user which terms the brain's
// `extract_terms` would pull from the given text. Diagnostic; helps users
// understand why specific files surface in @brain results.
COMMANDS.push({
  name: "extracted",
  description: "Show the terms the brain extracts from <text> (or current draft if empty)",
  usage: "[text]",
  run: async (a, ctx) => {
    const draft = (a ?? "").trim() ||
      "(empty — pass text after /extracted to test extraction)";
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const terms = await invoke<Array<{ text: string; boost: number }>>(
        "extract_terms_diagnostic",
        { message: draft },
      );
      if (terms.length === 0) {
        ctx.notify("/extracted", "No terms after stopword + length filters.", "info");
        return;
      }
      const lines = terms.map((t) => `  ${t.text} ×${t.boost.toFixed(1)}`).join("\n");
      ctx.append(
        systemNote(`**Extracted terms**\n\n\`\`\`\n${lines}\n\`\`\``),
      );
    } catch (e) {
      ctx.append(errorNote(`/extracted failed: ${humanizeError(e)}`));
    }
  },
});

// Wave 140 — `/import-chatgpt` opens the same file picker the Chats sidebar
// + ChatGPT button uses, then runs the Rust importer. Result toasted.
COMMANDS.push({
  name: "import-chatgpt",
  description: "Pick a ChatGPT conversations.json and import threads into Cortex",
  run: async (_a, ctx) => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        multiple: false,
        filters: [{ name: "ChatGPT export", extensions: ["json"] }],
      });
      if (!selected || typeof selected !== "string") return;
      ctx.notify("ChatGPT import started", "Parsing conversations.json…", "info");
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<{ imported: number; skipped: number; out_dir: string }>(
        "import_chatgpt_export",
        { path: selected },
      );
      ctx.notify(
        "ChatGPT import complete",
        `${result.imported} new, ${result.skipped} skipped → ${result.out_dir}`,
        "success",
      );
    } catch (e) {
      ctx.notify("/import-chatgpt failed", humanizeError(e), "error");
    }
  },
});

for (const tok of ["brain", "diff", "recent", "status", "repomap", "cwd", "env", "ls", "log"]) {
  COMMANDS.push({
    name: tok,
    description: `Attach @${tok} to the current draft (one-shot context from the brain / git)`,
    usage: tok === "brain" ? "[on|off]" : undefined,
    run: (a, ctx) => {
      const arg = (a ?? "").trim().toLowerCase();
      // `/brain on|off` flips the auto-trigger flag instead of inserting.
      if (tok === "brain" && (arg === "on" || arg === "off")) {
        ctx.store.getState().setBrainAutoEnabled(arg === "on");
        ctx.notify(`Brain auto-trigger ${arg === "on" ? "ON" : "OFF"}`, "", "info");
        return;
      }
      window.dispatchEvent(
        new CustomEvent("cortex:composer-insert", { detail: { value: `@${tok}` } }),
      );
    },
  });
}
rebuildSlashIndex();
