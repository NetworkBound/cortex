import { useEffect, useRef, useState } from "react";
import { useToastStore, type Toast } from "@/lib/toast";

export function ToastRack() {
  const toasts = useToastStore((s) => s.toasts);
  const dismiss = useToastStore((s) => s.dismissToast);

  if (toasts.length === 0) return null;
  return (
    <div className="toast-rack" role="region" aria-label="notifications">
      {toasts.map((t) => (
        <ToastCard key={t.id} toast={t} onDismiss={() => dismiss(t.id)} />
      ))}
    </div>
  );
}

function ToastCard({ toast, onDismiss }: { toast: Toast; onDismiss: () => void }) {
  const [paused, setPaused] = useState(false);
  const remainingRef = useRef<number>(toast.ttlMs);
  const startedAtRef = useRef<number>(Date.now());
  const timerRef = useRef<number | null>(null);

  useEffect(() => {
    if (paused) {
      // Pause: clear the active timer and snapshot remaining time.
      if (timerRef.current !== null) {
        window.clearTimeout(timerRef.current);
        timerRef.current = null;
      }
      const elapsed = Date.now() - startedAtRef.current;
      remainingRef.current = Math.max(0, remainingRef.current - elapsed);
      return;
    }
    // Resume (or initial start)
    startedAtRef.current = Date.now();
    const ttl = remainingRef.current;
    if (ttl <= 0) {
      onDismiss();
      return;
    }
    timerRef.current = window.setTimeout(() => {
      onDismiss();
    }, ttl);
    return () => {
      if (timerRef.current !== null) {
        window.clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [paused, onDismiss]);

  return (
    <div
      className={`toast toast-${toast.kind}`}
      role={toast.kind === "error" || toast.kind === "warning" ? "alert" : "status"}
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
    >
      <div className="toast-body">
        <div className="toast-title">{toast.title}</div>
        {toast.body && <div className="toast-text">{toast.body}</div>}
      </div>
      <button
        type="button"
        className="toast-close link-btn"
        aria-label="dismiss notification"
        onClick={onDismiss}
      >
        ✕
      </button>
    </div>
  );
}
