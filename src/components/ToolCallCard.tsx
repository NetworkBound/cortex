import { useState } from "react";
import type { ToolEvent } from "@/state/store";
import { Chevron } from "@/lib/chevron";

interface Props {
  tool: ToolEvent;
}

const STATUS_ICON: Record<ToolEvent["status"], string> = {
  running: "●",
  ok: "✓",
  error: "✕",
};

export function ToolCallCard({ tool }: Props) {
  const [open, setOpen] = useState(false);
  const dur = tool.duration_ms != null ? `${formatMs(tool.duration_ms)}` : "…";
  // Surface *what* the call acted on (file / path / args) inline on the
  // collapsed row — the Cursor / Cline / Claude.ai pattern — so the timeline
  // reads at a glance ("edit_file · src/api/client.ts") instead of a stack of
  // bare tool names you have to expand one by one. The full payload stays in
  // the expand-on-click preview below.
  const target = summarizePreview(tool.preview);
  return (
    <div className={`tool-card status-${tool.status}`}>
      <button className="tool-card-header" onClick={() => setOpen((o) => !o)}>
        <span className={`tool-icon status-${tool.status}`}>{STATUS_ICON[tool.status]}</span>
        <span className="tool-name">{tool.name}</span>
        {target && (
          <span className="tool-target" title={target}>{target}</span>
        )}
        <span className="tool-dur">{dur}</span>
        <span className="tool-chevron"><Chevron open={open} size={14} /></span>
      </button>
      {open && tool.preview && (
        <pre className="tool-preview">{tool.preview}</pre>
      )}
    </div>
  );
}

/** Condense a tool preview into a one-line target shown on the collapsed
 *  header: the first non-empty line, whitespace-collapsed. CSS handles the
 *  visual truncation (ellipsis) so it stays responsive to the card width;
 *  the hard cap here just bounds what we hand to the DOM for a huge blob. */
function summarizePreview(preview: string | null): string | null {
  if (!preview) return null;
  const firstLine = preview
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l.length > 0);
  if (!firstLine) return null;
  const collapsed = firstLine.replace(/\s+/g, " ");
  return collapsed.length > 200 ? `${collapsed.slice(0, 200)}…` : collapsed;
}

function formatMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(2)}s`;
}
