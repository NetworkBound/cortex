import { useEffect, useMemo, useRef, useState } from "react";
import { open as shellOpen } from "@tauri-apps/plugin-shell";
import { humanizeError } from "@/lib/errors";
import { useCortexStore, type ComposerEdit } from "@/state/store";
import {
  parseUnifiedDiff,
  sideBySideFromText,
  type DiffHunk,
  type SideBySideRow,
} from "@/lib/diff";
import { PinnedNotes } from "@/components/PinnedNotes";
import { Mic, Square, Loader2, Search, Zap } from "lucide-react";
import { getMode, setMode, type GatherMode } from "@/lib/gather-mode";
import {
  recordAndTranscribe,
  type RecordAndTranscribeHandle,
} from "@/lib/voice-fallback";
import { pushToast } from "@/lib/toast";

/**
 * Microphone capture button for the composer toolbar.
 *
 * Reuses the shipped `recordAndTranscribe()` helper (MediaRecorder →
 * whisper.cpp via the Rust `voice_transcribe` command) — the exact same
 * mechanism the `/voice` slash command's fallback path uses. On a successful
 * transcript we splice the text into the chat draft via the established
 * `cortex:composer-insert` window event (ChatPane listens for it; the same
 * channel FileExplorer/MemoryExplorer rows use), keeping this change additive
 * and contained to one file.
 *
 * States:
 *   - idle      → 🎤, click to start capture.
 *   - recording → pulsing red ⏹, click to stop early (a 4s safety timeout in
 *                 the helper also ends it).
 *   - busy      → ⏳ while whisper transcribes; button disabled.
 *
 * Failure modes (mic denied, MediaRecorder missing, whisper unavailable, empty
 * clip) surface a toast and reset the button — never throw.
 */
function MicCaptureButton() {
  type MicState = "idle" | "recording" | "busy";
  const [state, setState] = useState<MicState>("idle");
  const handleRef = useRef<RecordAndTranscribeHandle | null>(null);

  // Best-effort cleanup if the modal unmounts mid-recording so we don't pin
  // the mic open after the toolbar is gone.
  useEffect(() => {
    return () => {
      handleRef.current?.stop();
      handleRef.current = null;
    };
  }, []);

  const start = () => {
    let handle: RecordAndTranscribeHandle;
    try {
      handle = recordAndTranscribe();
    } catch (e) {
      pushToast({
        title: "Voice unavailable",
        body: humanizeError(e),
        kind: "warning",
      });
      return;
    }
    handleRef.current = handle;
    setState("recording");

    handle.promise.then(
      (transcript) => {
        handleRef.current = null;
        setState("idle");
        const text = transcript.trim();
        if (!text) {
          pushToast({ title: "Voice", body: "No speech captured.", kind: "info" });
          return;
        }
        // Splice into the chat composer draft via the shared insert channel.
        try {
          window.dispatchEvent(
            new CustomEvent("cortex:composer-insert", { detail: { value: text } }),
          );
        } catch {
          /* dispatch failures are non-fatal */
        }
      },
      (err) => {
        handleRef.current = null;
        setState("idle");
        pushToast({
          title: "Voice failed",
          body: humanizeError(err),
          kind: "warning",
        });
      },
    );

    // Once the user stops (or the safety timeout fires) the recorder enters its
    // onstop → whisper phase; flip to the busy spinner so the click target is
    // disabled while transcription is in flight.
    // We can't observe the exact stop moment without touching the helper, so we
    // transition to "busy" the instant stop() is invoked (see stop()).
  };

  const stop = () => {
    handleRef.current?.stop();
    // Recording has ended; whisper is now transcribing.
    setState("busy");
  };

  const onClick = () => {
    if (state === "idle") start();
    else if (state === "recording") stop();
    // "busy" → button is disabled, no-op.
  };

  const Icon = state === "recording" ? Square : state === "busy" ? Loader2 : Mic;
  const title =
    state === "recording"
      ? "Recording — click to stop (auto-stops after a few seconds)"
      : state === "busy"
        ? "Transcribing…"
        : "Record a voice clip and insert the transcript into the composer";

  // Recording state gets a red, pulsing treatment. We apply it inline (reusing
  // the existing global `pulse` keyframes by name) so the visual cue is
  // guaranteed without depending on a CSS edit in another agent's file.
  const recordingStyle =
    state === "recording"
      ? {
          color: "var(--danger)",
          fontWeight: 600,
          animation: "pulse 1s ease-in-out infinite",
        }
      : undefined;

  return (
    <button
      type="button"
      className={`link-btn composer-mic${state === "recording" ? " recording" : ""}`}
      style={recordingStyle}
      onClick={onClick}
      disabled={state === "busy"}
      aria-pressed={state === "recording"}
      aria-label={title}
      title={title}
    >
      <Icon
        size={14}
        strokeWidth={1.75}
        aria-hidden="true"
        style={state === "busy" ? { animation: "spin 0.8s linear infinite" } : undefined}
      />{" "}
      mic
    </button>
  );
}

interface FileGroup {
  path: string;
  edits: ComposerEdit[];
  totalLines: number;
  // The group's effective status:
  //   - "pending"  if any edit in the group is still pending
  //   - "rejected" if all edits are rejected (and none pending)
  //   - "accepted" if all edits are accepted (and none pending)
  status: ComposerEdit["status"];
  lastTs: number;
}

/** Hard cap on rows rendered before "show more" — keeps the modal snappy. */
const VISIBLE_ROW_LIMIT = 120;

function groupByPath(edits: ComposerEdit[]): FileGroup[] {
  const map = new Map<string, ComposerEdit[]>();
  for (const e of edits) {
    const arr = map.get(e.path);
    if (arr) arr.push(e);
    else map.set(e.path, [e]);
  }
  const out: FileGroup[] = [];
  for (const [path, group] of map.entries()) {
    const totalLines = group.reduce((acc, c) => acc + c.linesChanged, 0);
    const lastTs = group.reduce((acc, c) => (c.ts > acc ? c.ts : acc), 0);
    const hasPending = group.some((c) => c.status === "pending");
    const allAccepted = group.every((c) => c.status === "accepted");
    const allRejected = group.every((c) => c.status === "rejected");
    const status: ComposerEdit["status"] = hasPending
      ? "pending"
      : allAccepted
        ? "accepted"
        : allRejected
          ? "rejected"
          : "pending";
    out.push({ path, edits: group, totalLines, status, lastTs });
  }
  // Most recent activity first.
  out.sort((a, b) => b.lastTs - a.lastTs);
  return out;
}

function basename(p: string): string {
  const norm = p.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx >= 0 ? norm.slice(idx + 1) : norm;
}

function dirname(p: string): string {
  const norm = p.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx >= 0 ? norm.slice(0, idx) : "";
}

function looksLikeLocalPath(p: string): boolean {
  // Heuristic: absolute unix path, windows drive path, or starts with ~ or .
  return (
    p.startsWith("/") ||
    /^[A-Za-z]:[\\/]/.test(p) ||
    p.startsWith("~") ||
    p.startsWith("./") ||
    p.startsWith("../")
  );
}

/**
 * Big calm color-coded toggle pinned to the top of the composer modal.
 *
 * Two modes:
 *  - 🔍 **Gather** (blue) — read-only. Writes the read-only trust profile
 *    through `set_trust_matrix` so write tools / shell commands fall back
 *    to standard approval prompts.
 *  - ⚡ **Agent** (amber) — restores whatever trust profile the user had
 *    before they last entered Gather mode.
 *
 * Implementation lives in `@/lib/gather-mode`; this just renders the UI
 * and persists the active mode in the same place.
 */
function GatherAgentToggle() {
  const [mode, setLocal] = useState<GatherMode>(() => getMode());
  // Persisted mode is sync (localStorage) so the initial state above is
  // always correct, but we re-read on mount in case another tab/window
  // changed it under us.
  useEffect(() => {
    setLocal(getMode());
  }, []);

  const pick = async (next: GatherMode) => {
    if (next === mode) return;
    // Optimistically reflect the selection; setMode reverts via its own
    // toast on failure so the visual lie is short-lived.
    setLocal(next);
    await setMode(next);
  };

  return (
    <div className="gather-toggle" role="radiogroup" aria-label="Agent mode">
      <button
        type="button"
        className={`gather-toggle-btn gather${mode === "gather" ? " active" : ""}`}
        onClick={() => void pick("gather")}
        role="radio"
        aria-checked={mode === "gather"}
        title="Read-only — write tools and shell commands will ask first."
      >
        <span className="gather-toggle-icon" aria-hidden>
          <Search size={14} strokeWidth={1.75} />
        </span>
        <span className="gather-toggle-label">Gather</span>
        <span className="gather-toggle-hint">read-only</span>
      </button>
      <button
        type="button"
        className={`gather-toggle-btn agent${mode === "agent" ? " active" : ""}`}
        onClick={() => void pick("agent")}
        role="radio"
        aria-checked={mode === "agent"}
        title="Full write + execute — runs under your saved trust matrix."
      >
        <span className="gather-toggle-icon" aria-hidden>
          <Zap size={14} strokeWidth={1.75} />
        </span>
        <span className="gather-toggle-label">Agent</span>
        <span className="gather-toggle-hint">write + exec</span>
      </button>
    </div>
  );
}

function StatusIcon({ status }: { status: ComposerEdit["status"] }) {
  const symbol = status === "accepted" ? "✓" : status === "rejected" ? "✕" : "•";
  return <span className={`composer-status composer-status-${status}`}>{symbol}</span>;
}

/**
 * Render a unified-diff payload (`@@ ... @@` style) as a stack of hunks.
 * Each row is a single full-width line with a +/-/space gutter and the
 * appropriate color class. Truncated to `limit` rows total; the caller
 * adds the "Show more" affordance.
 */
function UnifiedDiffView({
  hunks,
  limit,
}: {
  hunks: DiffHunk[];
  limit: number;
}) {
  const rows: JSX.Element[] = [];
  let used = 0;
  for (const hunk of hunks) {
    if (used >= limit) break;
    rows.push(
      <div className="composer-diff-hunk-header" key={`h-${rows.length}`}>
        @@ -{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},{hunk.newCount} @@
        {hunk.header && hunk.header !== `@@ -${hunk.oldStart},${hunk.oldCount} +${hunk.newStart},${hunk.newCount} @@`
          ? ` ${hunk.header}`
          : ""}
      </div>,
    );
    for (const r of hunk.rows) {
      if (used >= limit) break;
      const marker = r.kind === "add" ? "+" : r.kind === "del" ? "-" : r.kind === "header" ? " " : " ";
      const cls =
        r.kind === "add"
          ? "add"
          : r.kind === "del"
            ? "del"
            : r.kind === "header"
              ? "header"
              : "ctx";
      rows.push(
        <div className={`composer-diff-row ${cls}`} key={`r-${rows.length}`}>
          <span className="composer-diff-gutter">
            {r.oldLine ?? ""}
          </span>
          <span className="composer-diff-gutter">
            {r.newLine ?? ""}
          </span>
          <span className="composer-diff-marker">{marker}</span>
          <span className="composer-diff-text">{r.text || " "}</span>
        </div>,
      );
      used += 1;
    }
  }
  return <div className="composer-diff composer-diff-unified">{rows}</div>;
}

/**
 * Render a side-by-side (old | new) view from raw file contents.
 * Pure-context rows span one column visually, but adds/dels/modifications
 * are split into two columns so the user can eyeball them.
 */
function SideBySideView({
  rows,
  limit,
}: {
  rows: SideBySideRow[];
  limit: number;
}) {
  const clipped = rows.slice(0, limit);
  return (
    <div className="composer-diff composer-diff-split">
      {clipped.map((r, i) => {
        const leftCls =
          r.kind === "context"
            ? "ctx"
            : r.kind === "add"
              ? "empty"
              : "del";
        const rightCls =
          r.kind === "context"
            ? "ctx"
            : r.kind === "del"
              ? "empty"
              : "add";
        return (
          <div className="composer-diff-row split" key={`s-${i}`}>
            <span className="composer-diff-gutter">{r.oldLine ?? ""}</span>
            <span className={`composer-diff-cell ${leftCls}`}>
              {r.oldText == null ? "" : r.oldText || " "}
            </span>
            <span className="composer-diff-gutter">{r.newLine ?? ""}</span>
            <span className={`composer-diff-cell ${rightCls}`}>
              {r.newText == null ? "" : r.newText || " "}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/**
 * Per-edit body: expand toggle + diff renderer or fallback preview chip.
 * Collapsed by default to keep the file list scannable.
 */
function EditDiffBody({ edit }: { edit: ComposerEdit }) {
  const [expanded, setExpanded] = useState(false);
  const [showAll, setShowAll] = useState(false);

  // Pick a render strategy ONCE per edit.
  const view = useMemo(() => {
    if (edit.diff && edit.diff.trim().length > 0) {
      const parsed = parseUnifiedDiff(edit.diff);
      if (parsed.totalRows > 0) {
        return { kind: "unified" as const, parsed };
      }
    }
    if (edit.oldContent != null && edit.newContent != null) {
      const rows = sideBySideFromText(edit.oldContent, edit.newContent);
      return { kind: "split" as const, rows };
    }
    return { kind: "none" as const };
  }, [edit.diff, edit.oldContent, edit.newContent]);

  if (view.kind === "none") {
    // Fallback: the original short preview chip.
    return (
      <div className="composer-edit-body">
        <span className="composer-edit-preview">
          +{edit.linesChanged} lines · no diff payload available
        </span>
      </div>
    );
  }

  const totalRows =
    view.kind === "unified" ? view.parsed.totalRows : view.rows.length;
  const visibleLimit = showAll ? totalRows : Math.min(totalRows, VISIBLE_ROW_LIMIT);
  const remaining = Math.max(0, totalRows - visibleLimit);

  return (
    <div className="composer-edit-body">
      <button
        className="link-btn composer-diff-toggle"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
      >
        {expanded ? "Hide diff" : "Show diff"}
        <span className="composer-diff-summary">
          · {totalRows} row{totalRows === 1 ? "" : "s"}
        </span>
      </button>
      {expanded && (
        <>
          {view.kind === "unified" ? (
            <UnifiedDiffView hunks={view.parsed.hunks} limit={visibleLimit} />
          ) : (
            <SideBySideView rows={view.rows} limit={visibleLimit} />
          )}
          {remaining > 0 && !showAll && (
            <button
              className="link-btn composer-diff-more"
              onClick={() => setShowAll(true)}
            >
              Show more ({remaining} more line{remaining === 1 ? "" : "s"})
            </button>
          )}
        </>
      )}
    </div>
  );
}

export function ComposerPanel() {
  const show = useCortexStore((s) => s.showComposer);
  const setShow = useCortexStore((s) => s.setShowComposer);
  const edits = useCortexStore((s) => s.composerEdits);
  const setStatus = useCortexStore((s) => s.setComposerEditStatus);
  const clearAll = useCortexStore((s) => s.clearComposerEdits);

  const groups = useMemo(() => groupByPath(edits), [edits]);
  const pendingCount = useMemo(
    () => edits.filter((e) => e.status === "pending").length,
    [edits],
  );

  if (!show) return null;

  const updateGroup = (group: FileGroup, status: ComposerEdit["status"]) => {
    for (const e of group.edits) {
      if (e.status === "pending") setStatus(e.id, status);
    }
  };

  const acceptAll = () => {
    for (const e of edits) {
      if (e.status === "pending") setStatus(e.id, "accepted");
    }
  };

  const rejectAll = () => {
    for (const e of edits) {
      if (e.status === "pending") setStatus(e.id, "rejected");
    }
  };

  const openInExternal = async (path: string) => {
    if (!looksLikeLocalPath(path)) return;
    try {
      await shellOpen(path);
    } catch {
      // best-effort; tauri shell scope may block it
    }
  };

  return (
    <div className="modal-backdrop composer-backdrop" onClick={() => setShow(false)}>
      <div
        className="modal composer-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label="Composer — multi-file edit review"
      >
        {/* Wave-4 additions: Gather/Agent mode switch + pinned-notes rail.
            Pinned at the top so they're always visible regardless of how
            much diff content scrolls below them. */}
        <div className="composer-top">
          <GatherAgentToggle />
          <PinnedNotes />
        </div>
        <div className="composer-header">
          <div className="composer-title">
            <h2>Composer</h2>
            <span className="composer-subtitle">
              {groups.length === 0
                ? "no pending edits"
                : `${groups.length} file${groups.length === 1 ? "" : "s"} · ${pendingCount} pending`}
            </span>
          </div>
          <div className="composer-toolbar">
            <MicCaptureButton />
            <button
              className="link-btn"
              onClick={acceptAll}
              disabled={pendingCount === 0}
              title="Mark every pending edit as accepted"
            >
              Accept all
            </button>
            <button
              className="link-btn danger"
              onClick={rejectAll}
              disabled={pendingCount === 0}
              title="Mark every pending edit as rejected"
            >
              Reject all
            </button>
            <button
              className="link-btn"
              onClick={() => {
                clearAll();
                setShow(false);
              }}
              disabled={edits.length === 0}
              title="Clear all review entries from this session"
            >
              Clear
            </button>
            <button className="link-btn" onClick={() => setShow(false)}>
              Close
            </button>
          </div>
        </div>

        <div className="composer-body">
          {groups.length === 0 ? (
            <div className="composer-empty">
              <div className="composer-empty-icon">∅</div>
              <div className="composer-empty-title">No pending file edits</div>
              <div className="composer-empty-hint">
                When an assistant edits files across this session, they appear here for review.
              </div>
            </div>
          ) : (
            <ul className="composer-list">
              {groups.map((g) => {
                const dir = dirname(g.path);
                const name = basename(g.path);
                const editCount = g.edits.length;
                const isPending = g.status === "pending";
                return (
                  <li
                    key={g.path}
                    className={`composer-row composer-row-${g.status}`}
                  >
                    <div className="composer-row-head">
                      <StatusIcon status={g.status} />
                      <div className="composer-row-main">
                        <div className="composer-row-name" title={g.path}>
                          {name}
                        </div>
                        <div className="composer-row-meta">
                          {dir && <span className="composer-row-dir">{dir}</span>}
                          <span className="composer-row-stats">
                            +{g.totalLines} lines
                            {editCount > 1 ? ` · ${editCount} edits` : ""}
                          </span>
                        </div>
                      </div>
                      <div className="composer-row-actions">
                        <button
                          className="link-btn"
                          onClick={() => updateGroup(g, "accepted")}
                          disabled={!isPending}
                          title="Accept this file's pending edits"
                        >
                          Accept
                        </button>
                        <button
                          className="link-btn danger"
                          onClick={() => updateGroup(g, "rejected")}
                          disabled={!isPending}
                          title="Reject this file's pending edits"
                        >
                          Reject
                        </button>
                        <button
                          className="link-btn"
                          onClick={() => void openInExternal(g.path)}
                          disabled={!looksLikeLocalPath(g.path)}
                          title={
                            looksLikeLocalPath(g.path)
                              ? "Open file in default editor"
                              : "Path does not resolve to a local file"
                          }
                        >
                          Open
                        </button>
                      </div>
                    </div>
                    <div className="composer-row-edits">
                      {g.edits.map((edit) => (
                        <EditDiffBody key={edit.id} edit={edit} />
                      ))}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div className="composer-footnote">
          Diffs render whatever the gateway streamed — unified patch, full
          before/after, or a count-only fallback chip.
        </div>
      </div>
    </div>
  );
}
