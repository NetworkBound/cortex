// Tool-call rows inside an assistant message: name + status chip + duration,
// args/result collapsed; tap opens a sheet with the full previews.
// Consecutive calls stack into one bordered group.

import { useEffect, useState } from "react";
import Icon from "./Icon";
import { Chip, Sheet } from "./ui";
import { fmtMs } from "../lib/format";
import type { ToolCall } from "../lib/types";

const STATUS_LABEL: Record<string, string> = {
  pending: "running",
  approved: "approved",
  denied: "denied",
  done: "done",
  error: "failed",
};

function tone(s: string): "ok" | "warn" | "err" | "info" | "muted" {
  switch (s) {
    case "done":
      return "ok";
    case "error":
    case "denied":
      return "err";
    case "pending":
      return "info";
    default:
      return "muted";
  }
}

function useNow(active: boolean) {
  const [, tick] = useState(0);
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(id);
  }, [active]);
}

export function ToolStack({ tools }: { tools: ToolCall[] }) {
  const [open, setOpen] = useState<ToolCall | null>(null);
  const anyPending = tools.some((t) => t.status === "pending");
  useNow(anyPending);
  if (tools.length === 0) return null;
  return (
    <>
      <div className="tool-stack">
        {tools.map((t) => {
          const dur =
            t.duration_ms ??
            (t.status === "pending" && t.started_ms
              ? Date.now() - t.started_ms
              : undefined);
          return (
            <button
              key={t.id}
              className={`tool-row st-${t.status}`}
              onClick={() => setOpen(t)}
              aria-label={`Tool ${t.name}, ${STATUS_LABEL[t.status] ?? t.status}`}
            >
              <span className="tool-name">
                <Icon name={t.name === "edit" ? "edit" : "bolt"} size={15} />
                {t.name}
              </span>
              {t.args_preview && (
                <span className="tool-args">{t.args_preview}</span>
              )}
              <span className="tool-right">
                {dur !== undefined && (
                  <span className="tool-dur">{fmtMs(dur)}</span>
                )}
                <Chip tone={tone(t.status)}>
                  {t.status === "pending" && <span className="spin small" />}
                  {STATUS_LABEL[t.status] ?? t.status}
                </Chip>
              </span>
            </button>
          );
        })}
      </div>
      <Sheet
        open={!!open}
        onClose={() => setOpen(null)}
        title={open?.name}
        tall
      >
        {open && (
          <div className="tool-detail">
            <div className="tool-detail-meta">
              <Chip tone={tone(open.status)}>
                {STATUS_LABEL[open.status] ?? open.status}
              </Chip>
              {open.duration_ms !== undefined && (
                <span className="muted">{fmtMs(open.duration_ms)}</span>
              )}
            </div>
            <div className="label">Arguments</div>
            <pre className="code">{open.args_preview || "—"}</pre>
            <div className="label">Result</div>
            <pre className="code">
              {open.result_preview ||
                (open.status === "pending" ? "Still running…" : "—")}
            </pre>
          </div>
        )}
      </Sheet>
    </>
  );
}
