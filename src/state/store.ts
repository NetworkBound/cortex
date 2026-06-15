import { create } from "zustand";
import { confirmDialog } from "@/lib/dialogs";
import { setCurrentMode as pushModeToBackend } from "@/lib/cortex-bridge";
import type { AgentDescriptor, ChatTurn, Risk } from "@/lib/cortex-bridge";
import type { ImageAttachment } from "@/lib/composer-drop";
import type { MultibufferExcerpt } from "@/lib/multibuffer";
import type { ProjectMeta } from "@/lib/projects";
import type { Profile } from "@/lib/profiles";
import { makeThread, newSessionId, patchActiveThread, type Thread } from "./threads";

export type { Thread } from "./threads";

export interface ToolEvent {
  id: string;
  agent: string;
  name: string;
  preview: string | null;
  status: "running" | "ok" | "error";
  duration_ms: number | null;
  startedAt: number;
  risk?: Risk;
  riskReason?: string;
}

/** An ordered slice of an assistant turn, recorded as it streams so the UI can
 *  render text and tool cards in the *temporal* order they arrived — the way
 *  Claude.ai / Cline / Cursor interleave narration and tool use — instead of
 *  collapsing every tool card above every paragraph. A `text` block holds a run
 *  of streamed tokens; a `tool` block references a `ToolEvent` by id in
 *  `Message.tools` (the canonical store, kept for history/copy/trace). */
export type MessageBlock =
  | { type: "text"; text: string }
  | { type: "tool"; toolId: string };

export interface PendingApproval {
  id: string;
  runId: string;
  agent: string;
  tool: string | null;
  preview: string | null;
  choices: string[];
  receivedAt: number;
  risk?: Risk;
  riskReason?: string;
  /** Raw tool-call payload from the backend `approval_request` event.
   *  Used by the approval UI for hunk-by-hunk review (when a `diff` field
   *  is present) and inline command editing (`command`/`cmd` field). */
  request?: unknown;
}

export interface Message {
  id: string;
  role: "user" | "assistant" | "system" | "error";
  agent?: string;
  content: string;
  reasoning?: string;
  pending?: boolean;
  tools: ToolEvent[];
  /** Ordered text/tool timeline for this turn, populated live as tokens and
   *  tool calls stream in. When present the chat renders blocks in order;
   *  when absent (legacy / rehydrated history) it falls back to the flat
   *  "all tools then all content" layout. See {@link MessageBlock}. */
  blocks?: MessageBlock[];
  approval?: PendingApproval | null;
  runId?: string | null;
  totalTokens?: number;
}

/** A composer submission captured while a turn was still streaming. Rendered
 *  as a pending "queued" bubble under the transcript and auto-dispatched (in
 *  FIFO order) by ChatPane's drain effect once the stream settles — the
 *  Cline/Cursor type-ahead behaviour. `content` is the RAW typed draft
 *  (pre-snippet-expansion); expansion happens at dispatch time so the message
 *  goes through the exact same pipeline as a live send. */
export interface QueuedMessage {
  id: string;
  content: string;
  images: ImageAttachment[];
  queuedAt: number;
}

export type Mode = "plan" | "act";

/** Agent-managed live to-do list item. See `lib/focus-chain.ts`. */
export interface FocusChainTask {
  id: string;
  title: string;
  done: boolean;
}

export type ActivityTab =
  | "brain"
  | "memory"
  | "sessions"
  | "projects"
  | "graph"
  | "agents"
  | "usage"
  | "observability"
  | "checkpoints"
  | "threads"
  | "focus"
  | "trust"
  | "skills"
  | "prp"
  | "terminal"
  | "git"
  | "source-control"
  | "editor"
  | "preview"
  | "orchestrator"
  | "tools"
  | "snippets"
  | "workflows"
  | "help"
  | "search"
  | "gateway"
  | "today"
  | "knowledge-graph"
  | "dep-graph"
  | "metrics"
  | "bookmarks"
  | "arena"
  | "channels"
  | "multibuffer"
  | "lanes"
  | "cookbook"
  | "research"
  | "routines"
  | "eval"
  | "setup"
  | null;

// `Thread` is defined in `./threads.ts` and re-exported above.

export interface ComposerEdit {
  id: string;
  path: string;
  status: "pending" | "accepted" | "rejected";
  linesChanged: number;
  ts: number;
  preview?: string | null;
  diff?: string;
  oldContent?: string;
  newContent?: string;
}

interface CortexState {
  // ── Thread multiplex ────────────────────────────────────────────────────
  // `threads` is the source of truth; the four legacy top-level fields below
  // (`sessionId`, `messages`, `runningRunIds`, `lastRoutingReason`) are kept
  // as live mirrors of the active thread for backwards compatibility with
  // existing components. Any write that touches one of those mirrors must
  // also write through to the matching thread, and vice versa.
  threads: Thread[];
  activeThreadId: string;

  sessionId: string;
  messages: Message[];
  agents: AgentDescriptor[];
  projects: ProjectMeta[];
  activeProject: ProjectMeta | null;
  hasApiKey: boolean;
  showSettings: boolean;
  showCommandPalette: boolean;
  runningRunIds: string[];
  lastRoutingReason: string | null;
  /** Type-ahead messages submitted while a turn was streaming, in send order.
   *  Drained (FIFO) by ChatPane once the stream settles and no approval is
   *  pending. Deliberately NOT per-thread: a queued message always dispatches
   *  into whatever thread is active when the drain fires, and reset/resume
   *  clear it so stale drafts never leak into a different conversation. */
  queuedMessages: QueuedMessage[];
  soundsEnabled: boolean;
  /** Provider/model ids the user selected to run a turn in parallel (Phase A
   * of the multi-provider feature). Empty = single-provider default routing. */
  selectedProviders: string[];
  expandedReasonings: Set<string>;
  activityTab: ActivityTab;
  showComposer: boolean;
  composerEdits: ComposerEdit[];
  showSessionPicker: boolean;
  showQuickOpen: boolean;
  currentWorktreeId: string | null;
  currentWorktreePath: string | null;
  onboardingComplete: boolean;
  /** Names of features the user has already seen / been shown a hint for.
   *  Used by the onboarding tour and Help panel so we don't replay a card
   *  the user dismissed. Persisted to localStorage. */
  seenFeatures: Set<string>;
  currentMode: Mode;
  currentProfile: Profile | null;
  /** Agent-managed live to-do list for the active session. */
  focusChain: FocusChainTask[];
  /** Aider-style `/architect` split: plan with one model, edit with another. */
  architectMode: boolean;
  /** Optional override for the planner phase model (only used when architectMode). */
  plannerModel: string | null;
  /** Optional override for the editor phase model (only used when architectMode). */
  editorModel: string | null;
  /** Per-prompt model override for normal chat. `null` = gateway default
   *  routing (`gateway-agent`). Set via the composer's model picker. */
  selectedModel: string | null;
  /** Per-prompt reasoning-effort override (`minimal | low | medium | high`,
   *  Codex CLI parity). `null` = use the global config default. Set via the
   *  composer's reasoning picker; forwarded by `chatSend`. */
  selectedReasoningEffort: string | null;
  /** Model ids selected for inline multi-model "compare" sends. Empty = off
   *  (single-model send using `selectedModel`). */
  compareModels: string[];
  /** Absolute path to the file currently open in the inline editor pane.
   *  `null` means the editor is in its empty-state. */
  editorPath: string | null;
  /** True while the inline editor holds unsaved changes. Maintained by
   *  EditorPane; `openEditorPath` consults it to confirm before a dirty
   *  buffer would be replaced or closed. */
  editorDirty: boolean;
  /** Currently selected localhost dev-server URL for the WebPreviewPane.
   *  `null` means "no server picked yet". */
  previewUrl: string | null;
  /** When `true`, the StatusBar hides secondary chips (gateway connection,
   *  project name, RepoWatch, msgs, session-id) so the bar shows only the
   *  PLAN/ACT toggle, security-critical Sandbox/notification badges,
   *  TokenHUD, and homelab health. Toggled via `Ctrl+.` and persisted to
   *  localStorage under `cortex.statusbar.compact`. */
  statusBarCompact: boolean;
  /** When false, the local brain DOES NOT auto-fire on typing pause.
   *  Users who find proactive suggestions intrusive can disable it via
   *  Settings. Persisted to localStorage under `cortex.brain.auto`. */
  brainAutoEnabled: boolean;
  setBrainAutoEnabled: (v: boolean) => void;
  /** When `true`, the conversation auto-condenses (real LLM summary of the
   *  older turns, the Cline "auto condense on overflow" behaviour) once the
   *  estimated live context crosses {@link autoCondenseThreshold} % of the
   *  model's context window. Persisted under `cortex.autocondense.enabled`. */
  autoCondenseEnabled: boolean;
  setAutoCondenseEnabled: (v: boolean) => void;
  /** Percent (0–100) of the model context window at which auto-condense fires.
   *  Persisted under `cortex.autocondense.threshold`. */
  autoCondenseThreshold: number;
  setAutoCondenseThreshold: (pct: number) => void;
  /** Zed-style multibuffer — N editable excerpts stitched into one tab.
   *  Source files are written back via `save_file_text`. Edits route
   *  through `setMultibufferExcerpts` so the React tree re-renders. */
  multibufferExcerpts: MultibufferExcerpt[];

  appendMessage: (m: Message) => void;
  appendTokenTo: (id: string, delta: string) => void;
  appendReasoningTo: (id: string, delta: string) => void;
  addToolStarted: (
    id: string,
    agent: string,
    tool: string,
    preview: string | null,
    risk?: Risk,
    riskReason?: string,
  ) => void;
  setToolFinished: (id: string, tool: string, ok: boolean, durationMs: number | null) => void;
  setApprovalOnMessage: (id: string, approval: PendingApproval | null) => void;
  setMessageRunId: (id: string, runId: string | null) => void;
  setMessageDone: (id: string) => void;
  setMessageError: (id: string, message: string) => void;
  resetSession: () => void;
  resumeSession: (sessionId: string, messages: Message[]) => void;
  /** Adopt a session id and/or message list onto the ACTIVE THREAD, keeping
   *  the thread record and the legacy top-level mirrors in lock-step. Use this
   *  instead of a raw `setState({ sessionId, messages })` — a bare setState
   *  writes only the mirrors, leaving the active thread record stale, so the
   *  very next `appendMessage` (via `patchActiveThread`) re-derives the mirrors
   *  from that stale thread and silently clobbers the adopted session/messages.
   *  That divergence is why a freshly-bootstrapped project chat couldn't send
   *  its first message. Pass only the fields you want to change. */
  adoptSession: (patch: { sessionId?: string; messages?: Message[] }) => void;
  setAgents: (a: AgentDescriptor[]) => void;
  setProjects: (p: ProjectMeta[]) => void;
  setActiveProject: (p: ProjectMeta | null) => void;
  setHasApiKey: (b: boolean) => void;
  setShowSettings: (b: boolean) => void;
  setShowCommandPalette: (b: boolean) => void;
  trackRunId: (runId: string) => void;
  untrackRunId: (runId: string) => void;
  /** Drop all tracked run ids for the active thread (turn fully finished). */
  clearRunIds: () => void;
  setLastRoutingReason: (s: string | null) => void;
  /** Append a type-ahead message to the send queue (composer submit while
   *  streaming). Order of enqueue == order of auto-dispatch. */
  enqueueMessage: (q: QueuedMessage) => void;
  /** Remove one queued message by id — the pending bubble's cancel ×, and the
   *  drain loop right before it dispatches (so a re-render can't double-send). */
  dequeueMessage: (id: string) => void;
  /** Drop every queued message (session reset / resume). */
  clearQueuedMessages: () => void;
  setSoundsEnabled: (b: boolean) => void;
  setSelectedProviders: (p: string[]) => void;
  /** Set the per-prompt chat model override (persisted). */
  setSelectedModel: (m: string | null) => void;
  /** Set the per-prompt reasoning-effort override (persisted). */
  setSelectedReasoningEffort: (e: string | null) => void;
  /** Replace the inline compare-model set (persisted). */
  setCompareModels: (m: string[]) => void;
  toggleReasoning: (id: string) => void;
  setActivityTab: (t: ActivityTab) => void;
  setShowComposer: (b: boolean) => void;
  addComposerEdit: (e: Omit<ComposerEdit, "id" | "ts" | "status"> & Partial<Pick<ComposerEdit, "id" | "ts" | "status">>) => void;
  setComposerEditStatus: (id: string, status: ComposerEdit["status"]) => void;
  clearComposerEdits: () => void;
  setShowSessionPicker: (b: boolean) => void;
  setShowQuickOpen: (b: boolean) => void;
  setCurrentWorktree: (id: string | null, path?: string | null) => void;
  setOnboardingComplete: (b: boolean) => void;
  /** Add `name` to `seenFeatures`. No-op if already present. */
  markFeatureSeen: (name: string) => void;
  setCurrentMode: (m: Mode) => void;
  setCurrentProfile: (p: Profile | null) => void;
  setFocusChain: (items: FocusChainTask[]) => void;
  setArchitectMode: (b: boolean) => void;
  setArchitectModels: (planner: string | null, editor: string | null) => void;
  openEditorPath: (path: string | null) => void;
  setEditorDirty: (dirty: boolean) => void;
  setPreviewUrl: (url: string | null) => void;
  setStatusBarCompact: (v: boolean) => void;
  setMultibufferExcerpts: (items: MultibufferExcerpt[]) => void;
  toHistory: () => ChatTurn[];

  // ── Thread actions ──────────────────────────────────────────────────────
  newThread: (label?: string) => string;
  switchThread: (id: string) => void;
  removeThread: (id: string) => void;
  /** Set (or clear, with "") the user-chosen custom title for a thread. */
  renameThread: (id: string, title: string) => void;
  /** Replace the in-memory thread list (used at boot to hydrate from disk). */
  hydrateThreads: (threads: Thread[], activeId?: string | null) => void;
  /** Direct-set the thread list without touching the per-message mirrors.
   *  Used by ThreadsList after a `list_threads` IPC roundtrip when we only
   *  want to refresh the picker, not switch the active thread. */
  setThreads: (items: Thread[]) => void;
  /** Direct-set the active id. Callers that also need the per-message
   *  mirrors to update should use `switchThread` instead. */
  setActiveThreadId: (id: string | null) => void;
  /** Returns the currently active thread, or `null` if somehow none exists. */
  getActiveThread: () => Thread | null;
}

const initialThread = makeThread({ label: "thread 1" });

export const useCortexStore = create<CortexState>((set, get) => ({
  threads: [initialThread],
  activeThreadId: initialThread.id,
  sessionId: initialThread.sessionId,
  messages: initialThread.messages,
  agents: [],
  projects: [],
  activeProject: null,
  hasApiKey: false,
  showSettings: false,
  showCommandPalette: false,
  runningRunIds: initialThread.runningRunIds,
  lastRoutingReason: initialThread.lastRoutingReason,
  queuedMessages: [],
  soundsEnabled: (() => {
    try { return localStorage.getItem("cortex.soundsEnabled") === "true"; } catch { return false; }
  })(),
  selectedProviders: (() => {
    try {
      const r = localStorage.getItem("cortex.selectedProviders");
      const v = r ? (JSON.parse(r) as unknown) : [];
      return Array.isArray(v) ? (v as string[]).filter((x) => typeof x === "string") : [];
    } catch { return []; }
  })(),
  expandedReasonings: new Set<string>(),
  activityTab: null,
  showComposer: false,
  composerEdits: [],
  showSessionPicker: false,
  showQuickOpen: false,
  currentWorktreeId: null,
  currentWorktreePath: null,
  onboardingComplete: (() => {
    try { return localStorage.getItem("cortex.onboarded") === "true"; } catch { return false; }
  })(),
  seenFeatures: (() => {
    try {
      const raw = localStorage.getItem("cortex.seenFeatures");
      if (!raw) return new Set<string>();
      const arr = JSON.parse(raw);
      return new Set<string>(Array.isArray(arr) ? arr : []);
    } catch { return new Set<string>(); }
  })(),
  currentMode: (() => {
    try {
      const v = localStorage.getItem("cortex.mode");
      return v === "plan" ? "plan" : "act";
    } catch { return "act"; }
  })(),
  currentProfile: null,
  focusChain: [],
  architectMode: (() => {
    try { return localStorage.getItem("cortex.architectMode") === "true"; } catch { return false; }
  })(),
  plannerModel: (() => {
    try { return localStorage.getItem("cortex.plannerModel"); } catch { return null; }
  })(),
  editorModel: (() => {
    try { return localStorage.getItem("cortex.editorModel"); } catch { return null; }
  })(),
  selectedModel: (() => {
    try { return localStorage.getItem("cortex.selectedModel"); } catch { return null; }
  })(),
  selectedReasoningEffort: (() => {
    try { return localStorage.getItem("cortex.selectedReasoningEffort"); } catch { return null; }
  })(),
  compareModels: (() => {
    try { return JSON.parse(localStorage.getItem("cortex.compareModels") || "[]"); } catch { return []; }
  })(),
  editorPath: null,
  editorDirty: false,
  previewUrl: null,
  statusBarCompact: (() => {
    try { return localStorage.getItem("cortex.statusbar.compact") === "true"; } catch { return false; }
  })(),
  brainAutoEnabled: (() => {
    try {
      const v = localStorage.getItem("cortex.brain.auto");
      // Default ON when unset.
      return v === null ? true : v === "true";
    } catch { return true; }
  })(),
  setBrainAutoEnabled: (v: boolean) => {
    try { localStorage.setItem("cortex.brain.auto", String(v)); } catch { /* private mode */ }
    set({ brainAutoEnabled: v });
  },
  autoCondenseEnabled: (() => {
    try {
      const v = localStorage.getItem("cortex.autocondense.enabled");
      // Default ON when unset — Cline condenses on overflow by default.
      return v === null ? true : v === "true";
    } catch { return true; }
  })(),
  setAutoCondenseEnabled: (v: boolean) => {
    try { localStorage.setItem("cortex.autocondense.enabled", String(v)); } catch { /* private mode */ }
    set({ autoCondenseEnabled: v });
  },
  autoCondenseThreshold: (() => {
    try {
      const raw = Number(localStorage.getItem("cortex.autocondense.threshold"));
      // Default 80% (aligns with the TokenHUD "Compact" affordance); clamp to a
      // sane band so a corrupt value can't disable or thrash the feature.
      if (!Number.isFinite(raw) || raw < 50 || raw > 95) return 80;
      return raw;
    } catch { return 80; }
  })(),
  setAutoCondenseThreshold: (pct: number) => {
    const clamped = Math.max(50, Math.min(95, Math.round(pct)));
    try { localStorage.setItem("cortex.autocondense.threshold", String(clamped)); } catch { /* private mode */ }
    set({ autoCondenseThreshold: clamped });
  },
  multibufferExcerpts: [],

  // Every per-message action below routes through `patchActiveThread` so the
  // matching thread record is updated AND the legacy top-level mirrors stay
  // in lock-step. Components reading either `state.messages` or
  // `getActiveThread().messages` always observe the same array.
  appendMessage: (m) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: [...t.messages, m],
        lastTs: Date.now(),
      })),
    ),
  appendTokenTo: (id, delta) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) => {
          if (m.id !== id) return m;
          // Coalesce into the trailing text block if the last thing to stream
          // was text; otherwise (turn start, or right after a tool call) open
          // a new text run so the block timeline stays in temporal order.
          const blocks = m.blocks ? [...m.blocks] : [];
          const last = blocks[blocks.length - 1];
          if (last && last.type === "text") {
            blocks[blocks.length - 1] = { type: "text", text: last.text + delta };
          } else {
            blocks.push({ type: "text", text: delta });
          }
          return { ...m, content: m.content + delta, blocks, pending: true };
        }),
        lastTs: Date.now(),
      })),
    ),
  appendReasoningTo: (id, delta) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) =>
          m.id === id ? { ...m, reasoning: (m.reasoning ?? "") + delta + "\n" } : m,
        ),
        lastTs: Date.now(),
      })),
    ),
  addToolStarted: (id, agent, tool, preview, risk, riskReason) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) => {
          if (m.id !== id) return m;
          const toolId = `t-${crypto.randomUUID()}`;
          return {
            ...m,
            tools: [
              ...m.tools,
              {
                id: toolId,
                agent,
                name: tool,
                preview,
                status: "running",
                duration_ms: null,
                startedAt: Date.now(),
                risk,
                riskReason,
              },
            ],
            // Record the tool in the ordered timeline so it renders where it
            // actually happened relative to the surrounding narration.
            blocks: [...(m.blocks ?? []), { type: "tool", toolId }],
          };
        }),
      })),
    ),
  setToolFinished: (id, tool, ok, durationMs) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) => {
          if (m.id !== id) return m;
          const rIdx = [...m.tools].reverse().findIndex((x) => x.name === tool && x.status === "running");
          if (rIdx < 0) return m;
          const realIdx = m.tools.length - 1 - rIdx;
          const updated = [...m.tools];
          updated[realIdx] = {
            ...updated[realIdx],
            status: ok ? "ok" : "error",
            duration_ms: durationMs,
          };
          return { ...m, tools: updated };
        }),
      })),
    ),
  setApprovalOnMessage: (id, approval) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) => (m.id === id ? { ...m, approval } : m)),
      })),
    ),
  setMessageRunId: (id, runId) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) => (m.id === id ? { ...m, runId } : m)),
      })),
    ),
  setMessageDone: (id) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) => (m.id === id ? { ...m, pending: false } : m)),
      })),
    ),
  setMessageError: (id, message) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        messages: t.messages.map((m) =>
          m.id === id
            ? { ...m, content: (m.content ? m.content + "\n\n" : "") + `error: ${message}`, role: "error", pending: false }
            : m,
        ),
      })),
    ),
  // Reset / resume both clear `focusChain` — the FocusChain component
  // rehydrates from disk via `loadFocusChain` after resumeSession returns.
  resetSession: () =>
    set((s) => ({
      ...patchActiveThread(s, (t) => ({
        ...t, sessionId: newSessionId(), messages: [], runningRunIds: [],
        lastRoutingReason: null, lastTs: Date.now(),
      })),
      focusChain: [],
      queuedMessages: [],
    })),
  resumeSession: (sessionId, messages) =>
    set((s) => ({
      ...patchActiveThread(s, (t) => ({
        ...t, sessionId, messages, runningRunIds: [],
        lastRoutingReason: null, lastTs: Date.now(),
      })),
      focusChain: [],
      queuedMessages: [],
    })),
  adoptSession: (patch) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        sessionId: patch.sessionId ?? t.sessionId,
        messages: patch.messages ?? t.messages,
        lastTs: Date.now(),
      })),
    ),
  setAgents: (a) => set({ agents: a }),
  setProjects: (p) => set({ projects: p }),
  setActiveProject: (p) => set({ activeProject: p }),
  setHasApiKey: (b) => set({ hasApiKey: b }),
  setShowSettings: (b) => set({ showSettings: b }),
  setShowCommandPalette: (b) => set({ showCommandPalette: b }),
  trackRunId: (runId) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        runningRunIds: [...t.runningRunIds, runId],
      })),
    ),
  untrackRunId: (runId) =>
    set((s) =>
      patchActiveThread(s, (t) => ({
        ...t,
        runningRunIds: t.runningRunIds.filter((r) => r !== runId),
      })),
    ),
  clearRunIds: () =>
    set((s) => patchActiveThread(s, (t) => ({ ...t, runningRunIds: [] }))),
  setLastRoutingReason: (reason) =>
    set((s) => patchActiveThread(s, (t) => ({ ...t, lastRoutingReason: reason }))),
  enqueueMessage: (q) =>
    set((s) => ({ queuedMessages: [...s.queuedMessages, q] })),
  dequeueMessage: (id) =>
    set((s) => ({ queuedMessages: s.queuedMessages.filter((q) => q.id !== id) })),
  clearQueuedMessages: () => set({ queuedMessages: [] }),
  setSoundsEnabled: (b) => {
    try { localStorage.setItem("cortex.soundsEnabled", String(b)); } catch { /* ignore */ }
    set({ soundsEnabled: b });
  },
  setSelectedProviders: (p) => {
    try { localStorage.setItem("cortex.selectedProviders", JSON.stringify(p)); } catch { /* ignore */ }
    set({ selectedProviders: p });
  },
  setSelectedModel: (m) => {
    try {
      if (m) localStorage.setItem("cortex.selectedModel", m);
      else localStorage.removeItem("cortex.selectedModel");
    } catch { /* ignore */ }
    set({ selectedModel: m });
  },
  setSelectedReasoningEffort: (e) => {
    try {
      if (e) localStorage.setItem("cortex.selectedReasoningEffort", e);
      else localStorage.removeItem("cortex.selectedReasoningEffort");
    } catch { /* ignore */ }
    set({ selectedReasoningEffort: e });
  },
  setCompareModels: (m) => {
    try { localStorage.setItem("cortex.compareModels", JSON.stringify(m)); } catch { /* ignore */ }
    set({ compareModels: m });
  },
  toggleReasoning: (id) =>
    set((s) => {
      const next = new Set(s.expandedReasonings);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return { expandedReasonings: next };
    }),
  setActivityTab: (t) => set({ activityTab: t }),
  setShowComposer: (b) => set({ showComposer: b }),
  addComposerEdit: (e) =>
    set((s) => ({
      composerEdits: [
        ...s.composerEdits,
        {
          id: e.id ?? `ce-${crypto.randomUUID()}`,
          ts: e.ts ?? Date.now(),
          status: e.status ?? "pending",
          path: e.path,
          linesChanged: e.linesChanged,
          preview: e.preview ?? null,
          diff: e.diff,
          oldContent: e.oldContent,
          newContent: e.newContent,
        },
      ],
    })),
  setComposerEditStatus: (id, status) =>
    set((s) => ({ composerEdits: s.composerEdits.map((e) => (e.id === id ? { ...e, status } : e)) })),
  clearComposerEdits: () => set({ composerEdits: [] }),
  setShowSessionPicker: (b) => set({ showSessionPicker: b }),
  setShowQuickOpen: (b) => set({ showQuickOpen: b }),
  setCurrentWorktree: (id, path) => set({ currentWorktreeId: id, currentWorktreePath: path ?? null }),
  setOnboardingComplete: (b) => {
    try { localStorage.setItem("cortex.onboarded", String(b)); } catch { /* ignore */ }
    set({ onboardingComplete: b });
  },
  markFeatureSeen: (name) =>
    set((s) => {
      if (s.seenFeatures.has(name)) return {};
      const next = new Set(s.seenFeatures);
      next.add(name);
      try {
        localStorage.setItem("cortex.seenFeatures", JSON.stringify([...next]));
      } catch { /* ignore */ }
      return { seenFeatures: next };
    }),
  setCurrentMode: (m) => {
    try { localStorage.setItem("cortex.mode", m); } catch { /* ignore */ }
    set({ currentMode: m });
    // Best-effort: push to backend so legacy chat_send callers respect it too.
    void pushModeToBackend(m).catch(() => { /* not in Tauri context */ });
  },
  setCurrentProfile: (p) => set({ currentProfile: p }),
  setFocusChain: (items) => set({ focusChain: items }),
  setArchitectMode: (b) => {
    try { localStorage.setItem("cortex.architectMode", String(b)); } catch { /* ignore */ }
    set({ architectMode: b });
  },
  setArchitectModels: (planner, editor) => {
    try {
      if (planner) localStorage.setItem("cortex.plannerModel", planner);
      else localStorage.removeItem("cortex.plannerModel");
      if (editor) localStorage.setItem("cortex.editorModel", editor);
      else localStorage.removeItem("cortex.editorModel");
    } catch { /* ignore */ }
    set({ plannerModel: planner, editorModel: editor });
  },
  openEditorPath: async (path) => {
    const { editorPath, editorDirty } = get();
    if (path === editorPath) return;
    // Dirty-buffer guard: with the editor kept alive across tab switches, the
    // only ways to lose unsaved edits are replacing the open file or closing
    // it — both funnel through here, so both get a confirm.
    if (editorDirty) {
      const name = editorPath?.split(/[/\\]/).pop() ?? "the open file";
      const ok = await confirmDialog({
        title: "Discard unsaved changes?",
        message: `${name} has unsaved edits that will be lost.`,
        confirmLabel: "Discard",
        danger: true,
      });
      if (!ok) return;
    }
    set({ editorPath: path, editorDirty: false });
  },
  setEditorDirty: (dirty) => {
    if (get().editorDirty !== dirty) set({ editorDirty: dirty });
  },
  setPreviewUrl: (url) => set({ previewUrl: url }),
  setStatusBarCompact: (v) => {
    try { localStorage.setItem("cortex.statusbar.compact", String(v)); } catch { /* ignore */ }
    set({ statusBarCompact: v });
  },
  setMultibufferExcerpts: (items) => set({ multibufferExcerpts: items }),
  toHistory: () =>
    get()
      .messages.filter((m) => m.role === "user" || m.role === "assistant")
      .map((m) => ({ role: m.role as "user" | "assistant", content: m.content, agent: m.agent })),

  // ── Thread actions ──────────────────────────────────────────────────────
  newThread: (label) => {
    const t = makeThread({
      label: label ?? `thread ${(get().threads.length ?? 0) + 1}`,
    });
    set((s) => ({
      threads: [...s.threads, t],
      activeThreadId: t.id,
      sessionId: t.sessionId,
      messages: t.messages,
      runningRunIds: t.runningRunIds,
      lastRoutingReason: t.lastRoutingReason,
    }));
    return t.id;
  },
  switchThread: (id) => {
    const target = get().threads.find((t) => t.id === id);
    if (!target) return;
    set({
      activeThreadId: target.id,
      sessionId: target.sessionId,
      messages: target.messages,
      runningRunIds: target.runningRunIds,
      lastRoutingReason: target.lastRoutingReason,
    });
  },
  removeThread: (id) => {
    set((s) => {
      const remaining = s.threads.filter((t) => t.id !== id);
      // Always keep at least one thread alive — if we just deleted the last
      // one, mint a fresh blank one so the UI never enters a "no thread"
      // state. Mirrors the way `resetSession` keeps a session alive.
      if (remaining.length === 0) {
        const fresh = makeThread({ label: "thread 1" });
        return {
          threads: [fresh],
          activeThreadId: fresh.id,
          sessionId: fresh.sessionId,
          messages: fresh.messages,
          runningRunIds: fresh.runningRunIds,
          lastRoutingReason: fresh.lastRoutingReason,
        };
      }
      // If the active thread got removed, fall back to the most-recent one.
      if (s.activeThreadId === id) {
        const next = remaining.slice().sort((a, b) => b.lastTs - a.lastTs)[0];
        return {
          threads: remaining,
          activeThreadId: next.id,
          sessionId: next.sessionId,
          messages: next.messages,
          runningRunIds: next.runningRunIds,
          lastRoutingReason: next.lastRoutingReason,
        };
      }
      return { threads: remaining };
    });
  },
  // Sets the user-facing custom title (ThreadsList inline rename). An empty
  // string clears the override so the title falls back to being derived from
  // the first message again.
  renameThread: (id, title) =>
    set((s) => ({
      threads: s.threads.map((t) =>
        t.id === id ? { ...t, customTitle: title.trim() || null } : t,
      ),
    })),
  hydrateThreads: (threads, activeId) => {
    if (threads.length === 0) return;
    const sorted = threads.slice().sort((a, b) => b.lastTs - a.lastTs);
    const active = (activeId && sorted.find((t) => t.id === activeId)) || sorted[0];
    set({
      threads: sorted,
      activeThreadId: active.id,
      sessionId: active.sessionId,
      messages: active.messages,
      runningRunIds: active.runningRunIds,
      lastRoutingReason: active.lastRoutingReason,
    });
  },
  setThreads: (items) => {
    // Merge by id so an incoming list (e.g. from `list_threads`) refreshes
    // metadata without clobbering the active thread's in-flight message
    // array. Threads not in the incoming list are dropped; new ones are
    // appended in the incoming order.
    set((s) => {
      const activeId = s.activeThreadId;
      const merged = items.map((t) => {
        if (t.id === activeId) {
          // Preserve the live message stream — disk copy may lag by up to
          // the autosave debounce window.
          const live = s.threads.find((x) => x.id === activeId);
          if (live) {
            return {
              ...t,
              messages: live.messages,
              runningRunIds: live.runningRunIds,
              lastRoutingReason: live.lastRoutingReason,
              lastTs: Math.max(t.lastTs, live.lastTs),
            };
          }
        }
        return t;
      });
      // If the active thread isn't in the incoming list (e.g. a brand new
      // unsaved thread), keep it in front so the user doesn't lose work.
      const hasActive = merged.some((t) => t.id === activeId);
      if (!hasActive && activeId) {
        const live = s.threads.find((x) => x.id === activeId);
        if (live) merged.unshift(live);
      }
      return { threads: merged };
    });
  },
  setActiveThreadId: (id) =>
    set((s) => {
      // `activeThreadId` must always reference a real thread: `patchActiveThread`
      // (and thus every message write) silently no-ops when it points at a
      // non-existent id. Coercing `null`/unknown ids to "" used to leave the
      // store in exactly that broken state, dropping writes with no signal.
      if (id != null && s.threads.some((t) => t.id === id)) {
        return { activeThreadId: id };
      }
      // Fall back to the most-recent thread so writes keep landing somewhere
      // valid. Warn loudly if a caller asked for a specific-but-missing id so
      // the empty-id state surfaces instead of failing silently.
      if (id != null) {
        console.warn(
          `setActiveThreadId: thread "${id}" not found; falling back to most-recent thread`,
        );
      }
      if (s.threads.length === 0) return { activeThreadId: "" };
      const next = s.threads.slice().sort((a, b) => b.lastTs - a.lastTs)[0];
      return {
        activeThreadId: next.id,
        sessionId: next.sessionId,
        messages: next.messages,
        runningRunIds: next.runningRunIds,
        lastRoutingReason: next.lastRoutingReason,
      };
    }),
  getActiveThread: () => {
    const s = get();
    return s.threads.find((t) => t.id === s.activeThreadId) ?? null;
  },
}));
