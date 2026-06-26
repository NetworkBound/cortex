/**
 * Inline assist popover — Ctrl/Cmd+I on a selection in the editor pane.
 *
 * Selection-anchored card with a one-line instruction input. Runs the
 * `inline_assist` backend command (adapter-registry routed, same as chat),
 * previews the rewrite as a del/add line diff, and applies it as a single
 * CodeMirror transaction so the edit is undo-able and flows through the
 * existing dirty/save pipeline. The buffer is never written to disk here.
 */
import { useEffect, useRef, useState } from "react";
import type { EditorView } from "@codemirror/view";
import { Loader2, Sparkles } from "lucide-react";

import { useCortexStore } from "@/state/store";
import { humanizeError } from "@/lib/errors";
import {
  assistContext,
  diffLines,
  runInlineAssist,
  type InlineAssistResult,
  type SelectionInfo,
} from "@/lib/editor-assist";

type Phase = "prompt" | "running" | "preview" | "error";

interface InlineAssistProps {
  view: EditorView;
  path: string;
  language: string;
  selection: SelectionInfo;
  /** Position relative to the editor body (already clamped by the caller). */
  anchor: { top: number; left: number };
  onClose: () => void;
}

export function InlineAssist({
  view,
  path,
  language,
  selection,
  anchor,
  onClose,
}: InlineAssistProps) {
  const [phase, setPhase] = useState<Phase>("prompt");
  const [instruction, setInstruction] = useState("");
  const [result, setResult] = useState<InlineAssistResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const applyRef = useRef<HTMLButtonElement | null>(null);
  // Bumped on close/resubmit so a stale in-flight completion can't land.
  const genRef = useRef(0);

  useEffect(() => {
    return () => {
      genRef.current += 1;
    };
  }, []);

  // Keep focus inside the popover so Esc/Enter always reach it — the input
  // is disabled while running (which drops focus to <body>), so re-grab it
  // on every phase change.
  useEffect(() => {
    if (phase === "preview") applyRef.current?.focus();
    else if (phase === "prompt" || phase === "error") inputRef.current?.focus();
  }, [phase]);

  async function submit() {
    const ask = instruction.trim();
    if (!ask || phase === "running") return;
    const gen = ++genRef.current;
    setPhase("running");
    setError(null);
    const ctx = assistContext(view, selection);
    try {
      const res = await runInlineAssist({
        selection: selection.text,
        before: ctx.before,
        after: ctx.after,
        language: language || null,
        instruction: ask,
        model: useCortexStore.getState().selectedModel,
        path: path || null,
      });
      if (gen !== genRef.current) return;
      setResult(res);
      setPhase("preview");
    } catch (err) {
      if (gen !== genRef.current) return;
      setError(humanizeError(err));
      setPhase("error");
    }
  }

  function apply() {
    if (!result) return;
    // The selection offsets were captured when the popover opened — refuse
    // to splice if the buffer moved underneath them.
    const current = view.state.sliceDoc(selection.from, selection.to);
    if (current !== selection.text) {
      setError("The buffer changed since you selected — reselect and try again.");
      setPhase("error");
      return;
    }
    view.dispatch({
      changes: { from: selection.from, to: selection.to, insert: result.replacement },
      selection: {
        anchor: selection.from,
        head: selection.from + result.replacement.length,
      },
      userEvent: "input.assist",
      scrollIntoView: true,
    });
    onClose();
    view.focus();
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      view.focus();
    } else if (e.key === "Enter" && phase === "preview") {
      e.preventDefault();
      apply();
    }
  }

  const rangeLabel =
    selection.startLine === selection.endLine
      ? `L${selection.startLine}`
      : `L${selection.startLine}–L${selection.endLine}`;
  const running = phase === "running";

  return (
    <div
      className="inline-assist"
      style={{ top: anchor.top, left: anchor.left }}
      onKeyDown={onKeyDown}
      role="dialog"
      aria-label="Inline assist"
    >
      <div className="inline-assist-head">
        <Sparkles size={13} strokeWidth={1.75} aria-hidden />
        <span className="inline-assist-title">Inline assist</span>
        <span className="inline-assist-range">{rangeLabel}</span>
        <span className="inline-assist-spacer" />
        <button
          className="inline-assist-close"
          onClick={() => {
            onClose();
            view.focus();
          }}
          aria-label="Close inline assist"
        >
          ×
        </button>
      </div>

      {(phase === "prompt" || phase === "running" || phase === "error") && (
        <form
          className="inline-assist-form"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <input
            ref={inputRef}
            className="inline-assist-input"
            placeholder="Edit the selection — e.g. “add error handling”"
            value={instruction}
            disabled={running}
            onChange={(e) => setInstruction(e.target.value)}
          />
          <button
            type="submit"
            className="inline-assist-run"
            disabled={running || !instruction.trim()}
          >
            {running ? (
              <>
                <Loader2 size={13} strokeWidth={2} className="inline-assist-spinner" aria-hidden />
                Rewriting…
              </>
            ) : (
              "Rewrite"
            )}
          </button>
        </form>
      )}

      {phase === "error" && error && (
        <div className="inline-assist-error" role="alert">
          {error}
        </div>
      )}

      {phase === "preview" && result && (
        <>
          <pre className="hunk-body inline-assist-diff">
            {diffLines(selection.text, result.replacement).map((row, i) => (
              <div key={i} className={`hunk-row hunk-row-${row.kind}`}>
                <span className="hunk-marker">
                  {row.kind === "add" ? "+" : row.kind === "del" ? "-" : " "}
                </span>
                <span className="hunk-text">{row.text}</span>
              </div>
            ))}
          </pre>
          <div className="inline-assist-actions">
            <span className="inline-assist-meta" title={`served by ${result.model}`}>
              {result.model} · {result.latency_ms}ms
            </span>
            <span className="inline-assist-spacer" />
            <button
              className="inline-assist-discard"
              onClick={() => {
                setResult(null);
                setPhase("prompt");
                setTimeout(() => inputRef.current?.focus(), 0);
              }}
            >
              Discard
            </button>
            <button ref={applyRef} className="inline-assist-apply" onClick={apply}>
              Apply
            </button>
          </div>
        </>
      )}
    </div>
  );
}
