import { useEffect, useRef, useState } from "react";
import { useDialogStore, type DialogRequest } from "@/lib/dialogs";

/**
 * Renders the head of the dialog queue (`lib/dialogs.ts`) as a token-styled
 * modal — the in-app stand-in for window.confirm/window.prompt. Mounted once
 * in App.tsx above everything except the toast rack.
 */
export function DialogHost() {
  const active = useDialogStore((s) => s.queue[0]);
  if (!active) return null;
  // Key by request id so a queued follow-up dialog remounts with fresh
  // input state instead of inheriting the previous one's.
  return <DialogCard key={active.id} request={active} />;
}

function DialogCard({ request }: { request: DialogRequest }) {
  const settle = useDialogStore((s) => s.settle);
  const isPrompt = request.kind === "prompt";
  const [value, setValue] = useState(
    isPrompt ? ((request.args as { initialValue?: string }).initialValue ?? "") : "",
  );
  const danger = !isPrompt && (request.args as { danger?: boolean }).danger === true;
  const inputRef = useRef<HTMLInputElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  // Guard against double-settles (Enter keydown + click racing, etc.).
  const settledRef = useRef(false);

  const finish = (v: boolean | string | null) => {
    if (settledRef.current) return;
    settledRef.current = true;
    settle(request.id, v);
  };
  const cancel = () => finish(isPrompt ? null : false);
  const confirm = () => finish(isPrompt ? value : true);

  useEffect(() => {
    // Initial focus: the input for prompts; Cancel for destructive confirms
    // (a stray Enter must not delete anything); Confirm otherwise.
    if (isPrompt) {
      inputRef.current?.focus();
      inputRef.current?.select();
    } else if (danger) {
      cancelRef.current?.focus();
    } else {
      confirmRef.current?.focus();
    }
  }, [isPrompt, danger]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        cancel();
      }
    };
    // Capture phase so the global shortcut handlers underneath never see the
    // Escape that closes a modal dialog.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const title =
    request.args.title ?? (isPrompt ? "Enter a value" : "Are you sure?");
  const confirmLabel =
    request.args.confirmLabel ?? (isPrompt ? "OK" : "Confirm");
  const cancelLabel = request.args.cancelLabel ?? "Cancel";
  const paragraphs = request.args.message.split("\n").filter((p) => p.trim() !== "");

  return (
    <div className="dialog-backdrop" onMouseDown={cancel}>
      <div
        className="dialog-card"
        role={isPrompt ? "dialog" : "alertdialog"}
        aria-modal="true"
        aria-label={title}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="dialog-title">{title}</div>
        <div className="dialog-message">
          {paragraphs.map((p, i) => (
            <p key={i}>{p}</p>
          ))}
        </div>
        {isPrompt && (
          <input
            ref={inputRef}
            className="dialog-input"
            type="text"
            value={value}
            placeholder={(request.args as { placeholder?: string }).placeholder}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                confirm();
              }
            }}
          />
        )}
        <div className="dialog-actions">
          <button type="button" ref={cancelRef} className="dialog-btn" onClick={cancel}>
            {cancelLabel}
          </button>
          <button
            type="button"
            ref={confirmRef}
            className={danger ? "dialog-btn btn-danger" : "dialog-btn btn-primary"}
            onClick={confirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
