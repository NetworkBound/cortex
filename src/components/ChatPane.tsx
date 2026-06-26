import { memo, useCallback, useEffect, useMemo, useRef, useState, type DragEvent, type ReactNode } from "react";
import { humanizeError } from "@/lib/errors";
import {
  Brain,
  Columns2,
  FileDiff,
  FileText,
  History,
  Paperclip,
  Settings,
  Sparkles,
  Wand2,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { chatSend, stopRun, subscribeToSession } from "@/lib/cortex-bridge";
import type { AgentEventEnvelope } from "@/lib/cortex-bridge";
import { useCortexStore, type Message, type ToolEvent } from "@/state/store";
import {
  extractImageAttachments,
  filesToComposerText,
  type ImageAttachment,
} from "@/lib/composer-drop";
import { loadPromptHistory, recordPrompt } from "@/lib/prompt-history";
import { MarkdownView } from "./MarkdownView";
import { MessageActions } from "./MessageActions";
import { ComposerPanel } from "./ComposerPanel";
import { ReasoningBlock } from "./ReasoningBlock";
import { ToolCallCard } from "./ToolCallCard";
import { AgentsDocChip } from "./AgentsDocChip";
import { ApprovalPrompt } from "./ApprovalPrompt";
import { FilePicker } from "./FilePicker";
import { ModelPicker } from "./ModelPicker";
import { ReasoningPicker } from "./ReasoningPicker";
import { listModels } from "@/lib/models";
import { arenaSend, formatLatency } from "@/lib/model-arena";
import { PlanCard } from "./PlanCard";
import { extractPlan } from "@/lib/plan";
import { playSound } from "@/lib/sounds";
import { pushToast } from "@/lib/toast";
import { confirmDialog, promptDialog } from "@/lib/dialogs";
import { evaluateBudget, type BudgetLevel } from "@/lib/budget";
import { formatUsd } from "@/lib/cost-tracker";
import { createCheckpoint, pruneCheckpoints } from "@/lib/checkpoints";
import { loadSessionMessages } from "@/lib/sessions";
import { recentIssues, recentCrashes } from "@/lib/observability";
import { expandSnippets, saveSnippet } from "@/lib/snippets";
import { findCommand, parseInput, makeContext } from "@/lib/slash-commands";
import { replaceChain } from "@/lib/focus-chain";
import {
  detectLanguageFromContent,
  shouldOfferSmartPaste,
  trimWhitespace,
  wrapInFence,
} from "@/lib/smart-paste";
import {
  gitWorkingDiff,
  projectDiagnostics,
  recentTerminalOutput,
} from "@/lib/context";
import {
  suggestContext,
  type ContextSuggestion,
} from "@/lib/context-picker";
import { SmartContextPrompt } from "./SmartContextPrompt";

// Cap pulled thread messages to keep model context lean.
const THREAD_MSG_CAP = 50;
// Cap stack trace lines on diagnostic insertions.
const DIAG_STACK_LINE_CAP = 200;

// Tool names that mutate the workspace — these trigger an auto-checkpoint
// after a successful invocation. Matched case-insensitively against the
// emitted tool_result name.
const WRITE_TOOL_RE = /(write|edit|patch|apply_patch|str_replace|create_file)/i;

export function ChatPane() {
  const sessionId = useCortexStore((s) => s.sessionId);
  const messages = useCortexStore((s) => s.messages);
  const activeProject = useCortexStore((s) => s.activeProject);
  const hasApiKey = useCortexStore((s) => s.hasApiKey);
  const append = useCortexStore((s) => s.appendMessage);
  const appendToken = useCortexStore((s) => s.appendTokenTo);
  const appendReasoning = useCortexStore((s) => s.appendReasoningTo);
  const addTool = useCortexStore((s) => s.addToolStarted);
  const finishTool = useCortexStore((s) => s.setToolFinished);
  const setApproval = useCortexStore((s) => s.setApprovalOnMessage);
  const setRunId = useCortexStore((s) => s.setMessageRunId);
  const setDone = useCortexStore((s) => s.setMessageDone);
  const setError = useCortexStore((s) => s.setMessageError);
  const toHistory = useCortexStore((s) => s.toHistory);
  const setShowSettings = useCortexStore((s) => s.setShowSettings);
  const setShowCommandPalette = useCortexStore((s) => s.setShowCommandPalette);
  const trackRunId = useCortexStore((s) => s.trackRunId);
  const untrackRunId = useCortexStore((s) => s.untrackRunId);
  const clearRunIds = useCortexStore((s) => s.clearRunIds);
  const runningRunIds = useCortexStore((s) => s.runningRunIds);
  const setLastRoutingReason = useCortexStore((s) => s.setLastRoutingReason);
  const lastRoutingReason = useCortexStore((s) => s.lastRoutingReason);
  const addComposerEdit = useCortexStore((s) => s.addComposerEdit);
  // Type-ahead queue (Cline/Cursor behaviour): messages submitted while a turn
  // streams are parked here, rendered as pending bubbles, and auto-dispatched
  // FIFO by the drain effect below once the stream settles.
  const queuedMessages = useCortexStore((s) => s.queuedMessages);
  const enqueueMessage = useCortexStore((s) => s.enqueueMessage);
  const dequeueMessage = useCortexStore((s) => s.dequeueMessage);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [images, setImages] = useState<ImageAttachment[]>([]);
  const [imageSkipped, setImageSkipped] = useState<string[]>([]);
  // Inline multi-model compare. `compareOn` toggles the chip row; the actual
  // model selection lives in the store (`compareModels`, persisted). When the
  // toggle is on AND ≥2 models are picked, `send()` routes through `arenaSend`
  // instead of the normal streaming path. `compareModelIds` is the available
  // model universe for the chip row (best-effort fetch, empty on failure).
  const [compareOn, setCompareOn] = useState(false);
  const [compareModelIds, setCompareModelIds] = useState<string[]>([]);
  const compareModels = useCortexStore((s) => s.compareModels);
  const setCompareModels = useCortexStore((s) => s.setCompareModels);
  // Composer drag-drop: `dragDepth` counts nested dragenter/leave so the
  // overlay doesn't flicker when the cursor crosses child elements.
  const [dragging, setDragging] = useState(false);
  const dragDepth = useRef(0);
  // @-picker state. `pickerQuery` is the text after the trailing `@` (everything
  // up to the cursor / next whitespace). Picker stays closed unless `pickerOpen`.
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerQuery, setPickerQuery] = useState("");
  const pickerAnchorRef = useRef<number>(-1);
  // Smart-context picker: AI-recommended `@`-tokens shown above the textarea
  // when the user clicks "🎯 Suggest context" or runs `/ctx`. Owned here so
  // the slash command can poke it via a window event.
  const [showContextPrompt, setShowContextPrompt] = useState(false);
  const [contextSuggestions, setContextSuggestions] = useState<
    ContextSuggestion[]
  >([]);
  const [suggestingContext, setSuggestingContext] = useState(false);
  const [brainThinking, setBrainThinking] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // Prompt-history recall (Up/Down in the composer). `histIndex` is null when
  // the user is editing their own live draft; once they start cycling we stash
  // that draft in `liveDraft` so ArrowDown past the newest entry restores it.
  const histIndexRef = useRef<number | null>(null);
  const liveDraftRef = useRef<string>("");
  const messagesEnd = useRef<HTMLDivElement>(null);
  const messagesContainer = useRef<HTMLDivElement>(null);
  // Stick-to-bottom: only auto-scroll on new content when the user is already
  // near the bottom. If they've scrolled up to read history mid-stream, leave
  // them there instead of yanking the viewport down on every token.
  const stickToBottom = useRef(true);
  const [showJumpToLatest, setShowJumpToLatest] = useState(false);
  const currentAssistantId = useRef<Map<string, string>>(new Map());
  // activeProject snapshot — read inside the subscription callback without
  // re-subscribing whenever the user switches projects mid-stream.
  const activeProjectRef = useRef(activeProject);
  useEffect(() => { activeProjectRef.current = activeProject; }, [activeProject]);
  // Tracks which assistant messages have already triggered an auto-checkpoint
  // so a flurry of write/edit calls in one turn collapses to one tarball.
  const checkpointPendingRef = useRef<Set<string>>(new Set());
  // Highest budget threshold already surfaced, so the 80%-warn toast fires
  // once per crossing instead of on every send/turn. Resets to "ok" when the
  // cap is raised/cleared (or spend otherwise drops back under).
  const budgetNotifiedRef = useRef<BudgetLevel>("ok");
  // Smart-paste menu. `pasted` is the original blob (so we can wrap/trim it
  // on demand); `start`/`end` mark where it landed in `input` so each action
  // can rewrite the slice in place. Null when the menu is hidden.
  const [smartPaste, setSmartPaste] = useState<{
    pasted: string;
    start: number;
    end: number;
    language: string;
  } | null>(null);
  const smartPasteTimerRef = useRef<number | null>(null);

  // Lazily fetch the model universe for the compare chip row, only once the
  // user opens the compare toggle. Best-effort: failure leaves the list empty.
  useEffect(() => {
    if (!compareOn || compareModelIds.length > 0) return;
    let alive = true;
    listModels()
      .then((list) => {
        if (alive) setCompareModelIds(list.map((m) => m.id));
      })
      .catch(() => {
        if (alive) setCompareModelIds([]);
      });
    return () => {
      alive = false;
    };
  }, [compareOn, compareModelIds.length]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let mounted = true;
    subscribeToSession(sessionId, (env: AgentEventEnvelope) => {
      if (!mounted) return;
      if (env.type === "orchestrator_route") {
        setLastRoutingReason(`routed → ${env.agents?.join(", ") ?? "?"} (${env.reason ?? ""})`);
        return;
      }
      const agentId = env.agent_id ?? "agent";
      const evt = env.event;
      if (!evt) return;

      const ensureAssistant = (): string => {
        let id = currentAssistantId.current.get(agentId);
        if (!id) {
          id = `m-${crypto.randomUUID()}`;
          currentAssistantId.current.set(agentId, id);
          append({
            id,
            role: "assistant",
            agent: agentId,
            content: "",
            pending: true,
            tools: [],
            runId: null,
          });
        }
        return id;
      };

      // Re-enable the composer only once every agent in this turn has emitted
      // its terminal event (done OR error). Keying off the live assistant map
      // (rather than the first `done`) keeps the composer disabled while
      // sibling agents in a multi-agent orchestrator run are still streaming,
      // and guarantees an error path re-enables it too. Also clears any run
      // ids left tracked (e.g. by an errored run that had no run_id to untrack).
      const finalizeIfIdle = () => {
        if (currentAssistantId.current.size === 0) {
          setSending(false);
          clearRunIds();
          // Surface a budget-threshold crossing as soon as the turn that
          // caused it finishes — the send-time gate alone would only tell
          // the user on their NEXT send.
          void (async () => {
            const budget = await evaluateBudget(sessionId || undefined);
            if (!budget) {
              budgetNotifiedRef.current = "ok";
              return;
            }
            if (budget.level !== "ok" && budget.level !== budgetNotifiedRef.current) {
              pushToast({
                title: budget.level === "over" ? "Budget cap reached" : "Approaching budget",
                body: `${formatUsd(budget.spent)} of the ${formatUsd(budget.cap)} cap (${Math.round(budget.pct * 100)}%).${budget.level === "over" ? " The next send will ask for confirmation." : ""}`,
                kind: budget.level === "over" ? "error" : "warning",
                ttlMs: 8000,
              });
            }
            budgetNotifiedRef.current = budget.level;
          })();
        }
      };

      switch (evt.type) {
        case "started": {
          const id = ensureAssistant();
          if (evt.run_id) {
            setRunId(id, evt.run_id);
            trackRunId(evt.run_id);
          }
          break;
        }
        case "token": {
          const id = ensureAssistant();
          appendToken(id, evt.delta);
          break;
        }
        case "reasoning": {
          const id = ensureAssistant();
          appendReasoning(id, evt.text);
          break;
        }
        case "tool_call": {
          const id = ensureAssistant();
          // Focus-chain hook: when the agent emits an `update_focus_chain`
          // tool call we route it into the focus-chain store mutator
          // instead of rendering as a generic tool card. The tool call
          // still shows in the trace, but the UI surface is the
          // dedicated FocusChain ActivityPanel tab.
          if (evt.name === "update_focus_chain") {
            try {
              // The agent emits either `args.items: [{title, done}, …]` or
              // a top-level array; accept both shapes.
              const raw = (evt.args as { items?: unknown } | unknown[]) ?? [];
              const items = Array.isArray(raw)
                ? raw
                : ((raw as { items?: unknown[] }).items ?? []);
              if (Array.isArray(items)) {
                replaceChain(
                  items.map((t) => {
                    const obj = t as { title?: string; done?: boolean; id?: string };
                    return {
                      id: obj.id,
                      title: String(obj.title ?? ""),
                      done: !!obj.done,
                    };
                  }),
                );
              }
            } catch (err) {
              console.warn("update_focus_chain parse failed", err);
            }
            // Still log as a tool so the trace remains complete.
            addTool(id, agentId, evt.name, evt.preview);
            break;
          }
          addTool(id, agentId, evt.name, evt.preview);
          break;
        }
        case "tool_result": {
          const id = ensureAssistant();
          finishTool(id, evt.name, evt.ok, evt.duration_ms);
          // Audible nudge on tool failure — gated by the user's sound prefs.
          if (!evt.ok) playSound("error");
          // Auto-checkpoint after a successful workspace-mutating tool call.
          // Throttled per-message so a stream of `edit` calls in one turn
          // produces a single tarball rather than 12. The next `done` event
          // resets the throttle so the *following* turn can checkpoint again.
          if (evt.ok && WRITE_TOOL_RE.test(evt.name)) {
            const root = activeProjectRef.current?.root;
            if (root && !checkpointPendingRef.current.has(id)) {
              checkpointPendingRef.current.add(id);
              void (async () => {
                try {
                  await createCheckpoint(root, "before next turn");
                  await pruneCheckpoints(root);
                } catch (err) {
                  console.warn("auto-checkpoint failed", err);
                  pushToast({
                    title: "Checkpoint failed",
                    body: "Workspace not snapshotted — recent edits aren't protected.",
                    kind: "error",
                  });
                }
              })();
            }
          }
          break;
        }
        case "file_edit": {
          const id = ensureAssistant();
          appendToken(id, `\n_edited ${evt.path} (${evt.lines_changed} lines)_\n`);
          // Record the edit so the Composer review modal can surface it.
          addComposerEdit({
            path: evt.path,
            linesChanged: evt.lines_changed,
          });
          break;
        }
        case "approval_request": {
          const id = ensureAssistant();
          setApproval(id, {
            id: `a-${crypto.randomUUID()}`,
            runId: evt.run_id,
            agent: agentId,
            tool: evt.tool,
            preview: evt.preview,
            choices: evt.choices,
            request: evt.request,
            receivedAt: Date.now(),
          });
          break;
        }
        case "approval_resolved": {
          const id = ensureAssistant();
          setApproval(id, null);
          break;
        }
        case "error": {
          const id = ensureAssistant();
          setError(id, evt.message);
          currentAssistantId.current.delete(agentId);
          // The error envelope carries no run_id, so we can't untrack the
          // specific run here; finalizeIfIdle clears any leftovers once every
          // agent has finished (otherwise the dead run lingers in
          // runningRunIds as a phantom "stop" button forever).
          finalizeIfIdle();
          break;
        }
        case "done": {
          const id = currentAssistantId.current.get(agentId);
          if (id) {
            setDone(id);
            playSound("done");
            // Reset the auto-checkpoint guard so the next turn is eligible.
            checkpointPendingRef.current.delete(id);
          }
          if (evt.run_id) untrackRunId(evt.run_id);
          currentAssistantId.current.delete(agentId);
          finalizeIfIdle();
          break;
        }
      }
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      mounted = false;
      unlisten?.();
    };
  }, [
    sessionId,
    append,
    appendToken,
    appendReasoning,
    addTool,
    finishTool,
    setApproval,
    setRunId,
    setDone,
    setError,
    trackRunId,
    untrackRunId,
    clearRunIds,
    setLastRoutingReason,
    addComposerEdit,
  ]);

  useEffect(() => {
    // Don't auto-scroll the empty-state landing: messagesEnd sits below the
    // tall hero card, so scrolling to it on mount clips the logo/wordmark off
    // the top (the empty state must stay anchored at its own top).
    if (!stickToBottom.current || messages.length === 0) return;
    messagesEnd.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, queuedMessages]);

  // Recompute stick-to-bottom on user scroll. Treat "within 80px of the
  // bottom" as pinned so a tiny mouse nudge doesn't unstick the stream.
  function onMessagesScroll() {
    const el = messagesContainer.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    const pinned = distance < 80;
    stickToBottom.current = pinned;
    setShowJumpToLatest(!pinned);
  }

  function jumpToLatest() {
    stickToBottom.current = true;
    setShowJumpToLatest(false);
    messagesEnd.current?.scrollIntoView({ behavior: "smooth" });
  }

  // Insert a `@token` at the current textarea cursor (or end of input if the
  // textarea isn't focused). A trailing space is appended so the next keystroke
  // doesn't accidentally extend the token.
  function insertAtCursor(token: string) {
    const ta = textareaRef.current;
    const insert = `${token} `;
    if (!ta) {
      setInput((cur) => `${cur}${cur && !cur.endsWith(" ") ? " " : ""}${insert}`);
      return;
    }
    const start = ta.selectionStart ?? input.length;
    const end = ta.selectionEnd ?? input.length;
    const before = input.slice(0, start);
    const after = input.slice(end);
    const needsLeadingSpace = before.length > 0 && !/\s$/.test(before);
    const piece = `${needsLeadingSpace ? " " : ""}${insert}`;
    const next = `${before}${piece}${after}`;
    setInput(next);
    // Restore caret just past the inserted token on the next paint.
    const caret = before.length + piece.length;
    setTimeout(() => {
      const t = textareaRef.current;
      if (!t) return;
      t.focus();
      t.setSelectionRange(caret, caret);
    }, 0);
  }

  // `/ctx` slash and the composer button both funnel through here. We keep the
  // card mounted (`showContextPrompt`) even when the list is empty so the user
  // sees a brief "no suggestions" before auto-dismiss kicks in.
  async function requestContextSuggestions() {
    if (suggestingContext) return;
    const draft = input.trim();
    if (!draft) return;
    setSuggestingContext(true);
    try {
      const out = await suggestContext(draft, activeProject?.root ?? null);
      setContextSuggestions(out);
      setShowContextPrompt(true);
    } catch (err) {
      console.warn("suggest_context failed", err);
    } finally {
      setSuggestingContext(false);
    }
  }

  // "Massive brain" auto-suggest. When the user pauses typing for 800ms
  // with a substantive draft (>= 25 chars), run the LOCAL brain (pure
  // grep + recency + source-kind scoring, target <2s) and surface the
  // top hits as @-token suggestions. While the grep is in flight, set
  // `brainThinking` so the composer can show a "💭 brain reading…" hint
  // — gives the user feedback that something is happening instead of
  // staring at a blank composer for 2 seconds.
  useEffect(() => {
    const draft = input.trim();
    if (draft.length < 25) return;
    if (!useCortexStore.getState().brainAutoEnabled) return;
    // Don't auto-fire if the user has already pinned 2+ context tokens —
    // they've expressed intent for specific context and probably don't
    // want more suggestions stacked on. One token is fine (they may want
    // a complementary attach).
    const tokenCount = (draft.match(/@(?:brain|diff|status|recent|repomap|cwd|env|ls|log)(?::[^\s,;)]*)?\b|@(?:memory|file|frag|web|grep|folder|dir|blame):|@[\/\\]/g) ?? []).length;
    // Wave 125 — implicit path mentions also count as "user expressed
    // context intent". If they typed `src/auth.rs` directly, the backend
    // will auto-attach it; the brain doesn't need to fan out further
    // unless the user has nothing else queued.
    const mentionCount = (draft.match(/\b[\w.\-]+(?:[\/\\][\w.\-]+)+\.(?:rs|ts|tsx|js|jsx|py|go|java|kt|c|cc|cpp|h|hpp|rb|php|swift|scala|md|toml|yaml|yml|json|css|scss|html|sh|sql|proto|gradle|zig|dart|elm|json5|lua|nix|tf|mjs|cjs|astro|vue|svelte|jl|ex|exs|clj|hs|ml)(?::\d+(?::\d+)?)?\b/g) ?? []).length;
    if (tokenCount + mentionCount >= 2) return;
    // Avoid re-firing if user already pinned an @-token from a prior brain
    // round — they don't need the same suggestions on subsequent keystrokes.
    const id = window.setTimeout(async () => {
      try {
        setBrainThinking(true);
        const out = await invoke<{ suggestions: Array<{ path: string; source: string; token: string; score: number; preview: string; matched_terms?: string[] }>, scanned_files: number, matched_files: number }>(
          "local_brain_suggest",
          { message: draft, projectRoot: activeProject?.root ?? null },
        );
        if (out.suggestions.length > 0) {
          setContextSuggestions(out.suggestions.map((s) => {
            // Wave 151 — surface the wave-150 matched_terms (top 3) so the
            // user sees WHICH parts of their draft pulled in this file.
            const matchedHint = (s.matched_terms && s.matched_terms.length > 0)
              ? ` · matched: ${s.matched_terms.slice(0, 3).join(", ")}`
              : "";
            return {
              kind: "memory" as const,
              value: s.path,
              reason: s.source + (s.preview ? ` · ${s.preview.slice(0, 60)}` : "") + matchedHint,
              confidence: Math.min(1, s.score / 8),
            };
          }));
          setShowContextPrompt(true);
        }
      } catch (err) {
        console.warn("local_brain_suggest failed", err);
      } finally {
        setBrainThinking(false);
      }
    }, 800);
    return () => window.clearTimeout(id);
     
  }, [input, activeProject?.root]);

  // FileExplorer + memory rows dispatch `cortex:composer-insert` with a
  // `value` payload (e.g. `@filename.md` or `@/abs/path`). Without this
  // listener, clicking a file was a silent no-op for the chat. Now the
  // token gets spliced at the cursor, ready to send as a task with that
  // file as @-context.
  useEffect(() => {
    const handler = (e: Event) => {
      const v = (e as CustomEvent<{ value?: string }>).detail?.value;
      if (typeof v === "string" && v.length > 0) insertAtCursor(v);
    };
    window.addEventListener("cortex:composer-insert", handler);
    return () => window.removeEventListener("cortex:composer-insert", handler);
    // insertAtCursor closes over `input` — re-bind on changes so cursor math is current.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [input]);

  // Hand-off hooks (e.g. Cookbook's "Use in chat") dispatch
  // `cortex:composer-focus` after selecting a model so the next keystroke
  // lands in the composer without an extra click.
  useEffect(() => {
    const handler = () => setTimeout(() => textareaRef.current?.focus(), 0);
    window.addEventListener("cortex:composer-focus", handler);
    return () => window.removeEventListener("cortex:composer-focus", handler);
  }, []);

  // Allow `/ctx` to drive the same flow from the slash registry — the command
  // dispatches a `cortex:suggest-context` event and we re-trigger here.
  useEffect(() => {
    const handler = () => void requestContextSuggestions();
    window.addEventListener("cortex:suggest-context", handler);
    return () => window.removeEventListener("cortex:suggest-context", handler);
    // requestContextSuggestions closes over `input` and `activeProject` —
    // re-bind whenever those change so the handler sees fresh values.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [input, activeProject?.root, suggestingContext]);

  // `cortex:chat-replay` — dispatched by ChatHistorySidebar, MemoryExplorer,
  // TraceDetail, and crash-viewer. Payload may carry `session_id` (load that
  // session's full history into the active chat) and/or `content`/`message`
  // (prefill the composer). Both behaviors compose: replaying a chat session
  // loads its messages AND optionally prefills the next user turn.
  useEffect(() => {
    const handler = (ev: Event) => {
      const detail = (ev as CustomEvent<{
        session_id?: string;
        content?: string;
        message?: string;
        file_path?: string;
      }>).detail;
      if (!detail) return;
      const prefill = detail.content ?? detail.message ?? "";
      // Load the full transcript and replace the in-memory message list.
      // Two paths: when the source is a Claude `.jsonl` chat we route through
      // `getClaudeChat(file_path)`; otherwise we hit the Cortex SQLite
      // `messages` table via `loadSessionMessages(session_id)`. Don't blank
      // the chat if the load returns empty — preserves the current chat
      // when a stale event fires with no real data.
      if (detail.file_path || detail.session_id) {
        void (async () => {
          try {
            let replayed: Message[] = [];
            // SQLite session is authoritative for in-app chats — try it
            // first. Only fall through to the raw `.jsonl` reader when
            // SQLite returns nothing (e.g. external Claude Code chats
            // that were never persisted via cortex's recordMessage).
            if (detail.session_id) {
              const msgs = await loadSessionMessages(detail.session_id);
              replayed = msgs.map((m) => ({
                id: `r-${crypto.randomUUID()}`,
                role: m.role as Message["role"],
                agent: m.agent_id ?? undefined,
                content: m.content,
                tools: [],
              }));
            }
            if (replayed.length === 0 && detail.file_path) {
              const { getClaudeChat } = await import("@/lib/chat-history");
              const transcript = await getClaudeChat(detail.file_path, 500);
              replayed = transcript.turns.map((t) => ({
                id: `r-${crypto.randomUUID()}`,
                role: t.role as Message["role"],
                content: t.content,
                tools: [],
              }));
            }
            if (replayed.length > 0) {
              // CRITICAL: also adopt the session_id so subsequent
              // `chat_send` calls thread into THIS session instead of
              // orphaning new messages onto the previous global one.
              // Adopt onto the active thread (record + mirrors together).
              // A bare setState would leave the thread record stale and the
              // next appendMessage would clobber the adopted session/messages.
              const patch: { sessionId?: string; messages: Message[] } = { messages: replayed };
              if (detail.session_id) patch.sessionId = detail.session_id;
              useCortexStore.getState().adoptSession(patch);

              // Auto-summarize on resume of long sessions. Fires in the
              // background (~2-15s via the gateway) and PREPENDS the summary as
              // a system message so the user sees "where we were" without
              // scrolling through 500 turns. Single shot per replay.
              if (detail.session_id && replayed.length >= 30) {
                const sid = detail.session_id;
                void (async () => {
                  try {
                    const { summarizeSession } = await import("@/lib/session-summary");
                    const out = await summarizeSession(sid, false);
                    const body = out?.body?.trim() ?? "";
                    const headline = out?.headline?.trim() ?? "";
                    if (body) {
                      const banner: Message = {
                        id: `summary-${crypto.randomUUID()}`,
                        role: "system",
                        content: `## Resumed session summary${headline ? ` — ${headline}` : ""}\n\n${body}`,
                        tools: [],
                      };
                      // Prepend through the active thread so the record and
                      // mirrors stay in lock-step (see adoptSession docs).
                      const st = useCortexStore.getState();
                      st.adoptSession({ messages: [banner, ...st.messages] });
                    }
                  } catch (err) {
                    console.warn("auto-summarize on resume failed", err);
                  }
                })();
              }
            }
          } catch (err) {
            console.warn("chat-replay load failed", err);
            pushToast({
              title: "Couldn't load that session",
              body: "The chat replay failed to load.",
              kind: "error",
            });
          }
        })();
      }
      if (prefill) {
        setInput((cur) => (cur ? cur + "\n\n" + prefill : prefill));
        setTimeout(() => textareaRef.current?.focus(), 0);
      }
    };
    window.addEventListener("cortex:chat-replay", handler);
    return () => window.removeEventListener("cortex:chat-replay", handler);
  }, []);

  // Core dispatch pipeline, shared by live sends and queued auto-sends.
  // Returns true when the message actually went out; false when a pre-send
  // gate stopped it (missing API key, over-budget confirm declined) so the
  // caller can keep/restore the draft instead of losing it. `onAccepted`
  // fires once every gate has passed — live sends clear the composer there
  // (NOT earlier), so cancelling the over-budget confirm keeps the draft.
  async function performSend(
    rawText: string,
    imgs: ImageAttachment[],
    onAccepted?: () => void,
  ): Promise<boolean> {
    if (!hasApiKey) {
      setShowSettings(true);
      return false;
    }
    // Budget gate (cap set via /budget): one-time toast when spend crosses
    // 80% of the cap, explicit confirm required for EVERY send past 100%.
    // Covers both the arena-compare and normal paths below. evaluateBudget
    // fails open (null) on a cost-estimate error so it can never wedge sends.
    const budget = await evaluateBudget(sessionId || undefined);
    if (budget) {
      if (budget.level === "over") {
        const proceed = await confirmDialog({
          title: "Over budget",
          message: `${formatUsd(budget.spent)} spent of the ${formatUsd(budget.cap)} cap (${Math.round(budget.pct * 100)}%).\nSend anyway? Raise or clear the cap with /budget.`,
          confirmLabel: "Send anyway",
          danger: true,
        });
        if (!proceed) return false;
      } else if (budget.level === "warn" && budgetNotifiedRef.current === "ok") {
        pushToast({
          title: "Approaching budget",
          body: `${formatUsd(budget.spent)} of the ${formatUsd(budget.cap)} cap (${Math.round(budget.pct * 100)}%). Adjust with /budget.`,
          kind: "warning",
          ttlMs: 6000,
        });
      }
      budgetNotifiedRef.current = budget.level;
    } else {
      budgetNotifiedRef.current = "ok";
    }
    onAccepted?.();
    // Snippet expansion: swap any `#snippet:name` markers for the saved
    // prompt body before the message hits the network. `expandSnippets`
    // returns the original text unchanged when no markers are present.
    const userMsg = await expandSnippets(rawText);
    const sendImages = imgs.map((img) => img.dataUrl);
    setSending(true);
    setLastRoutingReason(null);
    // Surface the attached images inline in the user's chat bubble so the
    // history transcript shows them even after they roll out of the model
    // context.
    const userContent =
      imgs.length > 0
        ? `${userMsg}${userMsg ? "\n\n" : ""}${imgs
            .map((i) => `![${i.name}](${i.dataUrl})`)
            .join("\n")}`
        : userMsg;
    append({
      id: `u-${crypto.randomUUID()}`,
      role: "user",
      content: userContent,
      tools: [],
    });

    // Inline multi-model compare: when the toggle is on and ≥2 models are
    // picked, fan the prompt out via `arenaSend` (parallel, non-streamed) and
    // render the collected responses as ONE assistant markdown message —
    // a `### {model}` section per turn. This deliberately bypasses the normal
    // streaming/routing path; single-model sends fall through unchanged.
    if (compareOn && compareModels.length >= 2) {
      try {
        const run = await arenaSend(userMsg, compareModels);
        const md = run.models
          .map((t) => {
            const body = t.error ? `> ⚠️ error: ${t.error}` : t.response || "_(empty response)_";
            const footer = `\n\n_${formatLatency(t.latency_ms)} · ${t.tokens} tokens_`;
            return `### ${t.model}\n\n${body}${footer}`;
          })
          .join("\n\n---\n\n");
        append({
          id: `m-${crypto.randomUUID()}`,
          role: "assistant",
          content: md || "_(no models responded)_",
          tools: [],
        });
      } catch (e) {
        append({ id: `m-${crypto.randomUUID()}`, role: "error", content: humanizeError(e), tools: [] });
      } finally {
        setSending(false);
      }
      return true;
    }

    try {
      const history = toHistory();
      const result = await chatSend({
        sessionId,
        message: userMsg,
        projectRoot: activeProject?.root,
        history,
        images: sendImages.length > 0 ? sendImages : undefined,
      });
      setLastRoutingReason(`routed → ${result.picked_agents.join(", ")} (${result.routing_reason})`);
      // Visible confirmation of what the brain actually attached. Without
      // this toast the user sends a message with `@brain` or `@diff` and
      // has no way to verify the backend resolved the tokens. Tooltip
      // shows the full list when truncated.
      const attached = result.attachments ?? [];
      if (attached.length > 0) {
        pushToast({
          title: `📎 attached ${attached.length} ${attached.length === 1 ? "block" : "blocks"}`,
          body: attached.join(", "),
          kind: "success",
          ttlMs: 3500,
        });
      }
    } catch (e) {
      const id = `m-${crypto.randomUUID()}`;
      append({ id, role: "error", content: humanizeError(e), tools: [] });
      setSending(false);
    }
    return true;
  }

  async function send() {
    if (!input.trim() && images.length === 0) return;
    // Typed slash commands execute locally — same registry + SlashContext the
    // command palette uses — instead of going to the model as chat text.
    // Unknown leading-slash input (absolute paths, prose) falls through to a
    // normal send, and an attached image always means "send to the model".
    // They run immediately even mid-stream: commands are local and never
    // touch the model turn, so there's nothing to queue behind.
    if (images.length === 0) {
      const typed = input.trim();
      const cmd = typed.startsWith("/") ? findCommand(typed) : null;
      if (cmd) {
        const args = parseInput(typed)?.args ?? "";
        recordPrompt(input);
        histIndexRef.current = null;
        setInput("");
        try {
          await cmd.run(args, makeContext());
        } catch (e) {
          append({
            id: `m-${crypto.randomUUID()}`,
            role: "error",
            content: `/${cmd.name} failed: ${humanizeError(e)}`,
            tools: [],
          });
        }
        return;
      }
    }
    // A turn is still streaming → park the submission in the type-ahead queue
    // (Cline/Cursor behaviour) instead of dropping it. It renders as a pending
    // bubble and the drain effect dispatches it, in order, once the stream
    // settles and no approval is pending.
    if (sending) {
      recordPrompt(input);
      histIndexRef.current = null;
      enqueueMessage({
        id: `q-${crypto.randomUUID()}`,
        content: input,
        images,
        queuedAt: Date.now(),
      });
      setInput("");
      setImages([]);
      setImageSkipped([]);
      return;
    }
    const draftText = input;
    const draftImages = images;
    await performSend(draftText, draftImages, () => {
      // Record the RAW typed draft (pre-snippet-expansion) for Up/Down recall,
      // then exit history-nav mode so the next send starts fresh. Runs only
      // once the gates pass — a declined over-budget confirm keeps the draft.
      recordPrompt(draftText);
      histIndexRef.current = null;
      setInput("");
      setImages([]);
      setImageSkipped([]);
    });
  }

  // ── Queue drain ──────────────────────────────────────────────────────────
  // Auto-dispatch queued type-ahead messages, in order, once the stream has
  // settled (`sending` false) AND no approval is pending — a paused approval
  // must stay the blocking decision; a queued message must never race past it.
  // (`sending` normally stays true through an approval pause since no `done`
  // arrives, but the explicit check also covers an errored run that left a
  // stale approval attached.) `drainingRef` bridges the async gap between
  // kicking off performSend and React flushing `sending=true`, so a re-render
  // mid-flight can't double-dispatch.
  const approvalPending = messages.some((m) => m.approval != null);
  const drainingRef = useRef(false);
  useEffect(() => {
    if (sending || approvalPending || drainingRef.current) return;
    if (queuedMessages.length === 0) return;
    const next = queuedMessages[0];
    drainingRef.current = true;
    // Dequeue BEFORE dispatching so a re-render can't pick it up twice.
    dequeueMessage(next.id);
    void (async () => {
      try {
        const dispatched = await performSend(next.content, next.images);
        if (!dispatched) {
          // A gate stopped it (no API key / over-budget declined): restore
          // the draft to the composer instead of silently dropping it.
          setInput((cur) => (cur ? `${cur}\n\n${next.content}` : next.content));
          if (next.images.length > 0) {
            setImages((cur) => [...cur, ...next.images]);
          }
        }
      } finally {
        drainingRef.current = false;
      }
    })();
    // performSend is re-created each render with fresh closures; the trigger
    // conditions below are the real dependencies.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sending, approvalPending, queuedMessages, dequeueMessage]);

  // Regenerate from a prior user turn. We own the send pipeline, so
  // MessageActions defers here: pre-fill the composer with the original
  // user message and focus it so the user can re-send (optionally tweaking
  // it first). Reuses the existing draft/focus path rather than duplicating
  // streaming/routing logic.
  // Stable identity so memoized MessageView bubbles don't re-render every turn.
  const regenerateFrom = useCallback((userContent: string) => {
    setInput(userContent);
    setTimeout(() => textareaRef.current?.focus(), 0);
  }, []);

  async function stopActive() {
    for (const rid of runningRunIds) {
      try { await stopRun(rid); } catch { /* ignore */ }
    }
  }

  // Track the trailing `@…` token in the composer. Opens the picker on `@`,
  // updates the query as the user types, and closes on whitespace / removal.
  function updatePickerFromInput(text: string, caret: number) {
    const upto = text.slice(0, caret);
    const atIdx = upto.lastIndexOf("@");
    if (atIdx < 0) {
      setPickerOpen(false);
      return;
    }
    // Must be start-of-string or preceded by whitespace.
    const prev = atIdx === 0 ? " " : upto[atIdx - 1];
    if (!/\s/.test(prev)) {
      setPickerOpen(false);
      return;
    }
    const token = upto.slice(atIdx + 1);
    // Whitespace inside the token terminates it.
    if (/\s/.test(token)) {
      setPickerOpen(false);
      return;
    }
    pickerAnchorRef.current = atIdx;
    setPickerQuery(token);
    setPickerOpen(true);
  }

  function onInputChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    const v = e.target.value;
    setInput(v);
    // Editing the text means we're no longer cycling history.
    histIndexRef.current = null;
    const caret = e.target.selectionStart ?? v.length;
    updatePickerFromInput(v, caret);
  }

  // Up/Down history recall. ArrowUp recalls a previous send when the caret is
  // at the very start of the composer (so it never hijacks normal line-up
  // navigation in a multi-line draft) or when already cycling; ArrowDown moves
  // forward, restoring the live draft past the newest entry. No-op with any
  // modifier or when there's no history.
  function tryHistoryNav(e: React.KeyboardEvent<HTMLTextAreaElement>): boolean {
    if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return false;
    if (e.shiftKey || e.metaKey || e.ctrlKey || e.altKey) return false;
    const ta = e.currentTarget;
    const atStart = ta.selectionStart === 0 && ta.selectionEnd === 0;
    const atEnd =
      ta.selectionStart === ta.value.length &&
      ta.selectionEnd === ta.value.length;
    const cycling = histIndexRef.current !== null;
    const hist = loadPromptHistory();
    if (hist.length === 0) return false;

    if (e.key === "ArrowUp") {
      if (!cycling && !atStart) return false;
      if (!cycling) {
        liveDraftRef.current = ta.value;
        histIndexRef.current = hist.length - 1;
      } else {
        histIndexRef.current = Math.max(0, (histIndexRef.current ?? 0) - 1);
      }
    } else {
      // ArrowDown only matters once we're cycling.
      if (!cycling) return false;
      if (!atEnd) return false;
      const next = (histIndexRef.current ?? 0) + 1;
      if (next >= hist.length) {
        histIndexRef.current = null;
        applyRecall(liveDraftRef.current);
        e.preventDefault();
        return true;
      }
      histIndexRef.current = next;
    }
    applyRecall(hist[histIndexRef.current]);
    e.preventDefault();
    return true;
  }

  // Swap the composer text and drop the caret at the end so the recalled prompt
  // is immediately editable / sendable.
  function applyRecall(text: string) {
    setInput(text);
    setTimeout(() => {
      const t = textareaRef.current;
      if (t) t.selectionStart = t.selectionEnd = t.value.length;
    }, 0);
  }

  // Inserts an envelope-aware payload at the `@…` anchor. For legacy `files`
  // values (plain filename) and unknown envelopes we substitute `@<value>`
  // verbatim. For `thread:` / `diagnostic:` we asynchronously expand into a
  // structured block appended to the composer (and strip the `@…` token).
  async function pickEntry(value: string) {
    setPickerOpen(false);
    const anchor = pickerAnchorRef.current;
    if (anchor < 0) return;

    // Slice out everything from `@` to the current end of the token.
    const before = input.slice(0, anchor);
    const after = input.slice(anchor);
    // The trailing token ends at first whitespace.
    const tokenEnd = after.search(/\s/);
    const tail = tokenEnd === -1 ? "" : after.slice(tokenEnd);

    const threadM = value.match(/^thread:(.+)$/);
    const diagM = value.match(/^diagnostic:(.+)$/);
    const diffM = value.match(/^diff:(.+)$/);
    const problemM = value.match(/^problem:(.+)$/);
    const terminalM = value.match(/^terminal:(.+)$/);

    if (diffM && activeProject) {
      const path = diffM[1];
      let block = `## Diff (${path})\n[unavailable]\n`;
      try {
        const full = await gitWorkingDiff(activeProject.root);
        // Slice out just this file's hunk (between `diff --git a/X b/Y` and the next one).
        const re = new RegExp(
          `(diff --git a/[^\\n]*${path.replace(/[.*+?^${}()|[\\]\\\\]/g, "\\\\$&")}[^\\n]*\\n[\\s\\S]*?)(?=\\ndiff --git |$)`,
          "m",
        );
        const m = full.match(re);
        block = `## Diff (${path})\n\`\`\`diff\n${(m?.[1] ?? full).slice(0, 12000)}\n\`\`\`\n`;
      } catch (err) {
        console.warn("@diff lookup failed", err);
      }
      const next = `${before}${block}${tail.trimStart() ? " " + tail.trimStart() : ""}`;
      setInput(next);
      setTimeout(() => textareaRef.current?.focus(), 0);
      return;
    }

    if (problemM && activeProject) {
      const id = problemM[1];
      let block = `## Problem ${id}\n[not found]\n`;
      try {
        const diags = await projectDiagnostics(activeProject.root);
        // The picker emits `index:` as the id (see at-vocab.ts diagnostics fetcher).
        const idx = Number.parseInt(id, 10);
        const d = Number.isFinite(idx) ? diags[idx] : null;
        if (d) {
          block =
            `## ${d.severity} ${d.source} ${d.path}:${d.line ?? "?"}\n` +
            `\`\`\`\n${d.message}\n\`\`\`\n`;
        }
      } catch (err) {
        console.warn("@problem lookup failed", err);
      }
      const next = `${before}${block}${tail.trimStart() ? " " + tail.trimStart() : ""}`;
      setInput(next);
      setTimeout(() => textareaRef.current?.focus(), 0);
      return;
    }

    if (terminalM) {
      let block = `## Terminal output\n[no recent shell output]\n`;
      try {
        const out = await recentTerminalOutput();
        if (out) {
          const trimmed = out.slice(-12000);
          block = `## Terminal output\n\`\`\`\n${trimmed}\n\`\`\`\n`;
        }
      } catch (err) {
        console.warn("@terminal lookup failed", err);
      }
      const next = `${before}${block}${tail.trimStart() ? " " + tail.trimStart() : ""}`;
      setInput(next);
      setTimeout(() => textareaRef.current?.focus(), 0);
      return;
    }

    if (threadM) {
      const sid = threadM[1];
      let block = `--- thread ${sid} ---\n[empty]\n--- end thread ---\n`;
      try {
        const msgs = await loadSessionMessages(sid);
        const capped = msgs.slice(-THREAD_MSG_CAP);
        const lines = capped
          .map((m) => `${m.role}: ${m.content}`)
          .join("\n");
        block = `--- thread ${sid} ---\n${lines}\n--- end thread ---\n`;
      } catch (err) {
        console.warn("loadSessionMessages failed", err);
      }
      const next = `${before}${block}${tail.trimStart() ? " " + tail.trimStart() : ""}`;
      setInput(next);
      setTimeout(() => textareaRef.current?.focus(), 0);
      return;
    }

    if (diagM) {
      const id = diagM[1];
      let block = `## Diagnostic\nid: ${id}\n[not found]\n`;
      try {
        const [issues, crashes] = await Promise.all([
          recentIssues(50),
          recentCrashes(50),
        ]);
        const issue = issues.find((i) => i.fingerprint === id);
        const crash = crashes.find((c) => String(c.id) === id);
        if (issue) {
          block =
            `## Diagnostic\n` +
            `kind: issue\n` +
            `message: ${issue.message}\n` +
            `error_class: ${issue.error_class ?? ""}\n` +
            `count: ${issue.count}\n`;
        } else if (crash) {
          const stack = (crash.stack ?? "")
            .split("\n")
            .slice(0, DIAG_STACK_LINE_CAP)
            .join("\n");
          block =
            `## Diagnostic\n` +
            `kind: ${crash.kind}\n` +
            `message: ${crash.message}\n` +
            `stack: ${stack}\n`;
        }
      } catch (err) {
        console.warn("diagnostic lookup failed", err);
      }
      const next = `${before}${block}${tail.trimStart() ? " " + tail.trimStart() : ""}`;
      setInput(next);
      setTimeout(() => textareaRef.current?.focus(), 0);
      return;
    }

    // Default: plain filename or `<kind>:<value>` envelope → inline `@value`.
    const insert = `@${value} `;
    const next = `${before}${insert}${tail.replace(/^\s+/, "")}`;
    setInput(next);
    setTimeout(() => {
      const t = textareaRef.current;
      if (!t) return;
      t.focus();
      const pos = before.length + insert.length;
      t.setSelectionRange(pos, pos);
    }, 0);
  }

  // ── Smart paste ───────────────────────────────────────────────────────────
  // Intercepts large pastes (>200 chars or >5 lines) and shows an inline
  // floating action menu under the textarea offering quick transforms. The
  // default action — Esc / outside-click / 8s timeout — is "paste as-is",
  // which is also what the browser already did by the time we render the
  // menu, so dismissing it is a true no-op.
  function dismissSmartPaste() {
    if (smartPasteTimerRef.current !== null) {
      window.clearTimeout(smartPasteTimerRef.current);
      smartPasteTimerRef.current = null;
    }
    setSmartPaste(null);
  }

  function rewriteSmartPasteSlice(replacement: string) {
    setSmartPaste((cur) => {
      if (!cur) return cur;
      setInput((prev) => {
        const before = prev.slice(0, cur.start);
        const after = prev.slice(cur.end);
        return `${before}${replacement}${after}`;
      });
      return null;
    });
    if (smartPasteTimerRef.current !== null) {
      window.clearTimeout(smartPasteTimerRef.current);
      smartPasteTimerRef.current = null;
    }
    setTimeout(() => textareaRef.current?.focus(), 0);
  }

  function onPasteCapture(e: React.ClipboardEvent<HTMLTextAreaElement>) {
    const pasted = e.clipboardData.getData("text");
    if (!pasted || !shouldOfferSmartPaste(pasted)) return;
    // Don't preventDefault — let the browser do the actual insertion. We just
    // remember where the blob landed so the menu actions can rewrite it.
    const t = e.currentTarget;
    const selStart = t.selectionStart ?? 0;
    const selEnd = t.selectionEnd ?? selStart;
    const start = selStart;
    const end = selStart + pasted.length;
    const language = detectLanguageFromContent(pasted);
    // Replacing a selection: the post-paste positions shift by (pasted.length
    // - (selEnd - selStart)), so the inserted blob still spans [start, end].
    void selEnd;
    if (smartPasteTimerRef.current !== null) {
      window.clearTimeout(smartPasteTimerRef.current);
    }
    setSmartPaste({ pasted, start, end, language });
    smartPasteTimerRef.current = window.setTimeout(() => {
      smartPasteTimerRef.current = null;
      setSmartPaste(null);
    }, 8000);
  }

  function smartPasteWrapFence() {
    if (!smartPaste) return;
    rewriteSmartPasteSlice(wrapInFence(smartPaste.pasted, smartPaste.language));
  }

  function smartPasteTrim() {
    if (!smartPaste) return;
    rewriteSmartPasteSlice(trimWhitespace(smartPaste.pasted));
  }

  async function smartPasteSaveAsSnippet() {
    if (!smartPaste) return;
    const name = await promptDialog({
      title: "Save as snippet",
      message: "Snippet name",
      placeholder: "e.g. retry-helper",
    });
    if (!name) return;
    const trimmed = name.trim();
    if (!trimmed) return;
    const saved = await saveSnippet(trimmed, smartPaste.pasted);
    if (saved) rewriteSmartPasteSlice(`#snippet:${trimmed}`);
    else dismissSmartPaste();
  }

  // ── Composer drag-drop ────────────────────────────────────────────────────
  // File-type handling lives in @/lib/composer-drop; this component owns only
  // the DOM event wiring and the insertion into the input state.
  function onDragEnter(e: DragEvent<HTMLDivElement>) {
    if (!e.dataTransfer.types.includes("Files")) return;
    e.preventDefault();
    dragDepth.current += 1;
    setDragging(true);
  }
  function onDragOver(e: DragEvent<HTMLDivElement>) {
    if (!e.dataTransfer.types.includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  }
  function onDragLeave(e: DragEvent<HTMLDivElement>) {
    if (!e.dataTransfer.types.includes("Files")) return;
    dragDepth.current = Math.max(0, dragDepth.current - 1);
    if (dragDepth.current === 0) setDragging(false);
  }
  async function onDrop(e: DragEvent<HTMLDivElement>) {
    if (!e.dataTransfer.files || e.dataTransfer.files.length === 0) return;
    e.preventDefault();
    dragDepth.current = 0;
    setDragging(false);
    // Split images out — they go to the vision-attachment chip rack, not into
    // the textarea (Terax #15). Everything else (text, code, unrecognized
    // binaries) still flows through `filesToComposerText`.
    const imageResult = await extractImageAttachments(e.dataTransfer.files, images);
    if (imageResult.attachments.length > 0) {
      setImages((cur) => [...cur, ...imageResult.attachments]);
    }
    setImageSkipped(imageResult.skipped);
    const nonImage = Array.from(e.dataTransfer.files).filter(
      (f) => !f.type.startsWith("image/"),
    );
    if (nonImage.length > 0) {
      const dt = new DataTransfer();
      for (const f of nonImage) dt.items.add(f);
      const text = await filesToComposerText(dt.files);
      if (text) {
        setInput((cur) => text + (cur ? "\n\n" + cur : ""));
      }
    }
    setTimeout(() => textareaRef.current?.focus(), 0);
  }

  return (
    <div className="chat-pane">
      <div className="chat-header">
        <div className="chat-header-left">
          <strong>{activeProject ? activeProject.name : "No project"}</strong>
          <AgentsDocChip />
          {lastRoutingReason && <span className="chat-routing">{lastRoutingReason}</span>}
        </div>
        <div className="chat-header-right">
          {runningRunIds.length > 0 && (
            <button className="link-btn danger" onClick={() => void stopActive()}>
              Stop ({runningRunIds.length})
            </button>
          )}
          <button
            type="button"
            className="chat-header-icon-btn"
            onClick={() => setShowSettings(true)}
            title="Settings"
            aria-label="Settings"
          >
            <Settings size={15} strokeWidth={1.75} aria-hidden="true" />
          </button>
        </div>
      </div>
      <div className="chat-messages" ref={messagesContainer} onScroll={onMessagesScroll}>
        {messages.length === 0 && (
          <div className="chat-empty">
            <div className="chat-empty-logo">C</div>
            <h2>Cortex</h2>
            <p className="chat-empty-sub">
              One chat, every model. Switch between Claude, Codex, Gemini, and more without leaving the conversation.
            </p>
            <ul className="chat-empty-tips">
              <li><span className="chat-empty-tip-label">Command palette</span><span className="chat-empty-keys"><kbd>Ctrl</kbd>+<kbd>K</kbd></span></li>
              <li><span className="chat-empty-tip-label">Send message</span><span className="chat-empty-keys"><kbd>Ctrl</kbd>+<kbd>Enter</kbd></span></li>
              <li><span className="chat-empty-tip-label">Search memory &amp; chats</span><span className="chat-empty-keys"><kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>F</kbd></span></li>
              <li><span className="chat-empty-tip-label">Toggle plan / act mode</span><span className="chat-empty-keys"><kbd>Ctrl</kbd>+<kbd>M</kbd></span></li>
            </ul>
            <div className="chat-empty-discover">
              {/* Calm single-line discovery nudge — the full @-token / slash
                  reference lives in the Help tab and the live @ / picker, so
                  the landing stays a brand-first hero (Linear/Raycast pattern)
                  instead of a reference card that overflowed the fold.
                  Clicking a <code> chip inserts that token in the composer. */}
              <p
                className="chat-empty-hint"
                onClick={(e) => {
                  const t = e.target as HTMLElement;
                  if (t.tagName !== "CODE") return;
                  const v = t.textContent ?? "";
                  if (!v) return;
                  if (v.startsWith("/")) {
                    setInput(v);
                  } else {
                    window.dispatchEvent(
                      new CustomEvent("cortex:composer-insert", { detail: { value: v } }),
                    );
                  }
                }}
              >
                Type <code>@</code> for context (<code>@brain</code>, <code>@diff</code>, <code>@status</code>…), <code>/</code> for commands.
              </p>
              <button
                type="button"
                className="chat-empty-browse"
                onClick={() => setShowCommandPalette(true)}
                title="Browse all features (Ctrl+K)"
              >
                Browse all features
                <span className="chat-empty-browse-hint">Ctrl+K</span>
              </button>
            </div>
          </div>
        )}
        {messages.map((m, i) => {
          // Group consecutive turns from the same author (role + agent) so the
          // role label shows once per author run, not on every message — the
          // standard chat-UI grouping (Claude.ai / ChatGPT / Slack / Linear).
          const prev = messages[i - 1];
          const continuesAuthor =
            !!prev && prev.role === m.role && prev.agent === m.agent;
          return (
            <MessageView
              key={m.id}
              m={m}
              setApproval={setApproval}
              onRegenerate={regenerateFrom}
              continuesAuthor={continuesAuthor}
            />
          );
        })}
        {/* Type-ahead queue: submissions parked while a turn streams. Pending
            bubbles, FIFO; each carries a cancel × and they auto-send (in
            order) via the drain effect when the stream settles. */}
        {queuedMessages.map((q) => (
          <div key={q.id} className="msg msg-user msg-queued">
            <div className="msg-role">
              <strong>user</strong>
              <span className="msg-queued-badge">queued</span>
            </div>
            <div className="msg-content">
              <span className="md-prose">{q.content}</span>
              {q.images.length > 0 && (
                <span className="msg-queued-images">
                  {q.images.length} image{q.images.length === 1 ? "" : "s"} attached
                </span>
              )}
            </div>
            <div className="msg-queued-foot">
              <span className="msg-queued-note">
                queued — sends when the current turn finishes
              </span>
              <button
                type="button"
                className="msg-queued-cancel"
                onClick={() => dequeueMessage(q.id)}
                title="Remove from queue"
                aria-label="Cancel queued message"
              >
                ×
              </button>
            </div>
          </div>
        ))}
        <div ref={messagesEnd} />
      </div>
      {showJumpToLatest && (
        <button
          type="button"
          className="chat-jump-latest"
          onClick={jumpToLatest}
          title="Scroll to the latest message"
        >
          ↓ latest
        </button>
      )}
      <div
        className={`chat-input${dragging ? " composer-dragging" : ""}`}
        onDragEnter={onDragEnter}
        onDragOver={onDragOver}
        onDragLeave={onDragLeave}
        onDrop={(e) => { void onDrop(e); }}
      >
        <div className="composer-drag-overlay" aria-hidden="true">
          drop files here
        </div>
        {(images.length > 0 || imageSkipped.length > 0) && (
          <div className="composer-images">
            {images.map((img) => (
              <div key={img.id} className="composer-image-chip" title={img.name}>
                <img
                  className="composer-image-thumb"
                  src={img.dataUrl}
                  alt={img.name}
                />
                <span className="composer-image-meta">
                  <span className="composer-image-name">{img.name}</span>
                  <span className="composer-image-size">
                    {(img.sizeBytes / 1024).toFixed(0)} KB
                  </span>
                </span>
                <button
                  type="button"
                  className="composer-image-remove"
                  onClick={() => setImages((cur) => cur.filter((i) => i.id !== img.id))}
                  title="Remove image"
                >
                  ×
                </button>
              </div>
            ))}
            {imageSkipped.map((s, i) => (
              <span key={`sk-${i}`} className="composer-image-skipped">
                {s}
              </span>
            ))}
          </div>
        )}
        <FilePicker
          open={pickerOpen}
          query={pickerQuery}
          onPick={(v) => void pickEntry(v)}
          onClose={() => setPickerOpen(false)}
        />
        {showContextPrompt && (
          <SmartContextPrompt
            suggestions={contextSuggestions}
            onInsert={(token) => insertAtCursor(token)}
            onInsertAll={(tokens) => {
              for (const t of tokens) insertAtCursor(t);
              setShowContextPrompt(false);
            }}
            onDismiss={() => setShowContextPrompt(false)}
          />
        )}
        <textarea
          ref={textareaRef}
          value={input}
          onChange={onInputChange}
          onKeyUp={(e) => {
            const t = e.currentTarget;
            updatePickerFromInput(t.value, t.selectionStart ?? t.value.length);
          }}
          onClick={(e) => {
            const t = e.currentTarget;
            updatePickerFromInput(t.value, t.selectionStart ?? t.value.length);
          }}
          onPaste={onPasteCapture}
          onKeyDown={(e) => {
            if (smartPaste && e.key === "Escape") {
              e.preventDefault();
              dismissSmartPaste();
              return;
            }
            // Terminal-style Up/Down recall of previous sends. Returns true
            // (and prevented-default) when it handled the key.
            if (!pickerOpen && tryHistoryNav(e)) return;
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void send();
            }
            // Brain quick-insert shortcuts. Alt+B/D/R/S/W splice
            // @brain / @diff / @recent / @status / @web: at the cursor.
            // Alt (not Ctrl) so the OS doesn't steal them — Ctrl+B in
            // many distros is a hotkey we don't want to clobber.
            if (e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
              const map: Record<string, string> = { b: "@brain", d: "@diff", r: "@recent", s: "@status", w: "@web:", c: "@cwd", e: "@env", g: "@grep:", m: "@repomap" };
              const tok = map[e.key.toLowerCase()];
              if (tok) { e.preventDefault(); insertAtCursor(tok); return; }
            }
            // Wave 131 — Alt+Shift+S replaces the whole composer with
            // `/summarize`. Hits the existing slash handler on Enter.
            if (e.altKey && e.shiftKey && !e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "s") {
              e.preventDefault();
              setInput("/summarize");
              return;
            }
            // Wave 211 — Alt+Shift+R for /repomap-top.
            if (e.altKey && e.shiftKey && !e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "r") {
              e.preventDefault();
              setInput("/repomap-top");
              return;
            }
          }}
          placeholder={
            hasApiKey
              ? sending
                ? "Type ahead — Ctrl+Enter queues it for when this turn finishes."
                : "Ask Cortex anything.  Ctrl+Enter to send."
              : "Add your API key in settings to begin."
          }
        />
        {smartPaste && (
          <div
            className="smart-paste-menu"
            role="menu"
            onMouseDown={(e) => e.preventDefault() /* keep textarea focus */}
          >
            <span className="smart-paste-label">
              Pasted {smartPaste.pasted.length} chars
              {smartPaste.language && ` · ${smartPaste.language}`}
            </span>
            <button
              type="button"
              className="smart-paste-action"
              onClick={smartPasteWrapFence}
              title="Wrap in code fence"
            >
              fence{smartPaste.language ? ` (${smartPaste.language})` : ""}
            </button>
            <button
              type="button"
              className="smart-paste-action"
              onClick={smartPasteTrim}
              title="Collapse blank lines and trailing whitespace"
            >
              trim
            </button>
            <button
              type="button"
              className="smart-paste-action"
              onClick={() => void smartPasteSaveAsSnippet()}
              title="Save the pasted text as a reusable snippet"
            >
              save snippet
            </button>
            <button
              type="button"
              className="smart-paste-action smart-paste-dismiss"
              onClick={dismissSmartPaste}
              title="Keep paste as-is (Esc)"
            >
              as-is
            </button>
          </div>
        )}
        <div className="chat-input-actions">
          <div className="chat-input-tools">
          <div className="quick-attach" role="toolbar" aria-label="Quick-attach context">
            <button
              type="button"
              className="quick-attach-btn"
              onClick={() => insertAtCursor("@brain")}
              title="Auto-attach top 3 brain hits for this message"
              aria-label="Attach top brain hits (@brain)"
            >
              <Brain size={14} strokeWidth={1.75} aria-hidden="true" /> brain
            </button>
            <button
              type="button"
              className="quick-attach-btn"
              onClick={() => insertAtCursor("@diff")}
              title="Attach git diff vs HEAD of active project"
              aria-label="Attach git diff (@diff)"
            >
              <FileDiff size={14} strokeWidth={1.75} aria-hidden="true" /> diff
            </button>
            <button
              type="button"
              className="quick-attach-btn"
              onClick={() => insertAtCursor("@recent")}
              title="Attach last 8 modified files in active project"
              aria-label="Attach recent files (@recent)"
            >
              <History size={14} strokeWidth={1.75} aria-hidden="true" /> recent
            </button>
            <button
              type="button"
              className="quick-attach-btn"
              onClick={() => setInput("/summarize")}
              aria-label="Drop slash-summarize into composer"
              title="Drop /summarize into the composer; press Enter to run"
            >
              <FileText size={14} strokeWidth={1.75} aria-hidden="true" /> summary
            </button>
          </div>
          {brainThinking && (
            <span className="brain-thinking" title="Local brain greping memory + recent edits for relevant @-context">
              <Sparkles size={14} strokeWidth={1.75} aria-hidden="true" /> brain reading…
            </span>
          )}
          {(() => {
            // Pre-send attachment preview. Counts the @-tokens in the
            // current draft that the backend `expand_at_tokens` will
            // resolve, so the user can see "📎 3 attachments queued"
            // before hitting send. Matches the same shape regex as the
            // backend: special tokens (@diff/@status/@recent/@brain),
            // @memory:<abs>, @file:<abs>, @<abs>.
            const tokens = input.match(/@(?:brain|diff|status|recent|repomap|cwd|env|ls|log)(?::[^\s,;)]*)?\b|@(?:memory|file|frag|web|grep|folder|dir|blame):[^\s,;)]+|@[\/\\][^\s]+/g) ?? [];
            // Wave 120 — Aider-style implicit path mentions (no `@`). Same
            // criteria the backend uses in `expand_at_tokens`: relative path
            // with a known code/.md extension, max 3.
            const mentionMatches = input.match(/\b[\w.\-]+(?:[\/\\][\w.\-]+)+\.(?:rs|ts|tsx|js|jsx|py|go|java|kt|c|cc|cpp|h|hpp|rb|php|swift|scala|md|toml|yaml|yml|json|css|scss|html|sh|sql|proto|gradle|zig|dart|elm|json5|lua|nix|tf|mjs|cjs|astro|vue|svelte|jl|ex|exs|clj|hs|ml)(?::\d+(?::\d+)?)?\b/g) ?? [];
            const mentions = mentionMatches.slice(0, 3);
            const total = tokens.length + mentions.length;
            if (total === 0) return null;
            return (
              <span
                className="attach-preview"
                title={[...tokens, ...mentions].join(" ")}
              >
                <Paperclip size={14} strokeWidth={1.75} aria-hidden="true" /> {total} attachment{total === 1 ? "" : "s"} queued
              </span>
            );
          })()}
          <button
            type="button"
            className="link-btn smart-context-trigger"
            onClick={() => void requestContextSuggestions()}
            disabled={sending || suggestingContext || !input.trim()}
            title="Ask AI which @-tokens to attach"
          >
            <Wand2 size={14} strokeWidth={1.75} aria-hidden="true" />
            {suggestingContext ? "thinking…" : "Suggest context"}
          </button>
          <ModelPicker />
          <ReasoningPicker />
          <button
            type="button"
            className={`link-btn compare-toggle${compareOn ? " on" : ""}`}
            onClick={() => setCompareOn((v) => !v)}
            aria-pressed={compareOn}
            title="Compare the same prompt across multiple models side-by-side"
          >
            <Columns2 size={14} strokeWidth={1.75} aria-hidden="true" /> compare
          </button>
          </div>
          <button
            className="btn-primary chat-send"
            onClick={() => void send()}
            disabled={!input.trim()}
            title={
              sending
                ? "Queue this message — it sends automatically when the current turn finishes"
                : "Send (Ctrl+Enter)"
            }
          >
            {sending ? "Queue" : "Send"}
          </button>
        </div>
        {compareOn && (
          <div className="compare-chips" role="group" aria-label="Compare models (pick 2–4)">
            {compareModelIds.length === 0 ? (
              <span className="compare-hint">no models available</span>
            ) : (
              compareModelIds.map((id) => {
                const selected = compareModels.includes(id);
                // Cap selection at 4; disable unselected chips once full.
                const atCap = compareModels.length >= 4;
                return (
                  <button
                    key={id}
                    type="button"
                    className={`compare-chip${selected ? " selected" : ""}`}
                    aria-pressed={selected}
                    disabled={!selected && atCap}
                    onClick={() =>
                      setCompareModels(
                        selected
                          ? compareModels.filter((m) => m !== id)
                          : [...compareModels, id],
                      )
                    }
                  >
                    {id}
                  </button>
                );
              })
            )}
            {compareModelIds.length > 0 && (
              <span className="compare-hint">
                {compareModels.length < 2
                  ? "pick ≥2 to compare"
                  : `comparing ${compareModels.length} model${compareModels.length === 1 ? "" : "s"}`}
              </span>
            )}
          </div>
        )}
      </div>
      {/* Multi-file edit review modal — self-gates on `showComposer`. */}
      <ComposerPanel />
    </div>
  );
}

// Render one run of assistant/user text: a PlanCard when it's a well-formed
// plan (Cline / Aider plan mode), otherwise markdown for the assistant and
// plain prose for the user. Shared by the flat fallback and the block timeline.
function renderTextContent(m: Message, text: string, key?: string): ReactNode {
  if (m.role === "assistant") {
    const plan = extractPlan(text);
    if (plan) {
      return (
        <div className="msg-content" key={key}>
          <PlanCard plan={plan} sessionId={m.id} />
        </div>
      );
    }
  }
  // Render markdown for every app-authored voice — the assistant AND the
  // system/error notes (`/test`, `/lint`, `/architect`, snapshot, repo-map…),
  // which are written WITH markdown (inline `code`, **bold**, fenced output
  // blocks, bullet lists). They previously fell through to the plain-text span
  // and leaked literal backticks/asterisks/fences into the chat stream — a
  // visible amateur tell. Only the user's own typed turn stays verbatim (we
  // don't reinterpret what they typed, and the attachment-chip parsing relies
  // on the raw content).
  const asMarkdown = m.role !== "user";
  return (
    <div className="msg-content" key={key}>
      {asMarkdown ? (
        <MarkdownView source={text} />
      ) : (
        <span className="md-prose">{text}</span>
      )}
    </div>
  );
}

// Render a message body. When an ordered block timeline is present (live or
// rehydrated turns), interleave text runs and tool cards in the order they
// streamed — narration sits above the tools it introduces, summaries below the
// tools they describe — matching Claude.ai / Cline / Cursor. Consecutive tool
// blocks are grouped into one card stack. Otherwise fall back to the flat
// "all tools, then all content" layout used by legacy messages.
function renderTimeline(m: Message): ReactNode {
  if (!m.blocks || m.blocks.length === 0) {
    return (
      <>
        {m.tools.length > 0 && (
          <div className="msg-tools">
            {m.tools.map((t) => <ToolCallCard key={t.id} tool={t} />)}
          </div>
        )}
        {m.content ? renderTextContent(m, m.content) : null}
      </>
    );
  }
  const out: ReactNode[] = [];
  for (let i = 0; i < m.blocks.length; ) {
    const b = m.blocks[i];
    if (b.type === "text") {
      if (b.text.trim()) out.push(renderTextContent(m, b.text, `t${i}`));
      i++;
    } else {
      const group: ToolEvent[] = [];
      while (i < m.blocks.length) {
        const bk = m.blocks[i];
        if (bk.type !== "tool") break;
        const tool = m.tools.find((t) => t.id === bk.toolId);
        if (tool) group.push(tool);
        i++;
      }
      if (group.length > 0) {
        out.push(
          <div className="msg-tools" key={`g${i}`}>
            {group.map((t) => <ToolCallCard key={t.id} tool={t} />)}
          </div>,
        );
      }
    }
  }
  return <>{out}</>;
}

// Memoized: with stable setApproval/onRegenerate props, only the message whose
// object actually changed re-renders. During streaming that's just the one
// in-flight bubble — prior bubbles no longer re-parse markdown per token.
const MessageView = memo(function MessageView({
  m,
  setApproval,
  onRegenerate,
  continuesAuthor = false,
}: {
  m: Message;
  setApproval: (id: string, a: null) => void;
  onRegenerate: (userContent: string) => void;
  // True when the previous message is from the same author (role + agent), so
  // we suppress the repeated role label and tighten the gap — the message
  // grouping every mature chat UI uses (Claude.ai / ChatGPT / Slack / Linear).
  continuesAuthor?: boolean;
}) {
  // Extract @-token chips for user messages so users can see at a glance
  // what context was attached on each turn. The persisted content is the
  // ORIGINAL (pre-expansion) typed message, so we re-parse the same
  // patterns the backend's `expand_at_tokens` recognises.
  const attachmentChips: string[] = useMemo(() => {
    if (m.role !== "user" || !m.content) return [];
    const re = /@(?:brain|diff|status|recent|repomap|cwd|env|ls|log)(?::[^\s,;)]*)?\b|@(?:memory|file|frag|web|grep|blame):[^\s,;)]+|@[\/\\][^\s,;)]+/g;
    const matches: string[] = [];
    let mm: RegExpExecArray | null;
    while ((mm = re.exec(m.content)) !== null) {
      // Compact label — last segment for paths so the chip stays short.
      const tok = mm[0];
      if (tok.includes(":")) {
        const [head, ...rest] = tok.split(":");
        const tail = rest.join(":");
        const short = tail.split(/[\/\\]/).pop() ?? tail;
        matches.push(`${head}:${short}`);
      } else if (tok.startsWith("@/") || tok.startsWith("@\\")) {
        const short = tok.split(/[\/\\]/).pop() ?? tok;
        matches.push(`@${short}`);
      } else {
        matches.push(tok);
      }
      if (matches.length >= 8) break;
    }
    // Wave 121 — also surface implicit path mentions (no @-prefix) the
    // backend resolved. Match same regex shape as wave 120 / backend,
    // capped at 3 to mirror the cap in `expand_at_tokens`.
    const mentionRe = /\b[\w.\-]+(?:[\/\\][\w.\-]+)+\.(?:rs|ts|tsx|js|jsx|py|go|java|kt|c|cc|cpp|h|hpp|rb|php|swift|scala|md|toml|yaml|yml|json|css|scss|html|sh|sql|proto|gradle|zig|dart|elm|json5|lua|nix|tf|mjs|cjs|astro|vue|svelte|jl|ex|exs|clj|hs|ml)(?::\d+(?::\d+)?)?\b/g;
    let mn: RegExpExecArray | null;
    let mentionCount = 0;
    while ((mn = mentionRe.exec(m.content)) !== null) {
      const tail = mn[0].split(/[\/\\]/).pop() ?? mn[0];
      matches.push(`📎 ${tail}`);
      mentionCount += 1;
      if (mentionCount >= 3) break;
      if (matches.length >= 11) break;
    }
    return matches;
  }, [m.content, m.role]);
  return (
    <div className={`msg msg-${m.role}${continuesAuthor ? " msg-cont" : ""}`}>
      {!continuesAuthor ? (
        <div className="msg-role">
          <strong>{m.agent ?? m.role}</strong>
          {m.pending && <span className="cursor"> ▎</span>}
        </div>
      ) : (
        // Grouped follow-up: the author label is suppressed, but keep the
        // streaming cursor visible while this turn is still generating.
        m.pending && <span className="cursor msg-cont-cursor">▎</span>
      )}
      {attachmentChips.length > 0 && (
        <div className="msg-attachments" role="list" aria-label="Brain attachments on this message">
          {attachmentChips.map((c, i) => {
            // Wave 121 implicit-mention chips are prefixed with `📎 ` in
            // the data array; wave 137 amber-tints them via data-mention.
            const isMention = c.startsWith("📎 ");
            const label = isMention ? c.slice(2) : c;
            // Wave 157 — tooltip on amber implicit-mention chips shows the
            // actual basename + a hint, so a reviewer hovering over chips
            // months later can tell at a glance which were implicit.
            return (
              <span
                key={i}
                className="msg-attachment-chip"
                role="listitem"
                data-mention={isMention ? "true" : undefined}
                title={isMention ? `Auto-attached via implicit path mention: ${label}` : undefined}
              >
                <Paperclip size={12} strokeWidth={1.75} aria-hidden="true" /> {label}
              </span>
            );
          })}
        </div>
      )}
      {m.reasoning && (
        <ReasoningBlock reasoning={m.reasoning} messageId={m.id} />
      )}
      {renderTimeline(m)}
      {m.approval && (
        <ApprovalPrompt
          approval={m.approval}
          onResolved={() => setApproval(m.id, null)}
        />
      )}
      <MessageActions message={m} onRegenerate={onRegenerate} />
    </div>
  );
});
