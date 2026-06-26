/**
 * Live linting overlay for the CodeMirror editor (Cursor/VS Code style).
 *
 * Wraps `projectDiagnostics()` from `@/lib/context` as a CodeMirror `linter()`
 * source. Diagnostics come from `cargo check` / `tsc --noEmit` and are cached
 * server-side for 30s, so polling on a 1.5s idle delay is cheap.
 *
 * Render targets:
 *   - inline squiggly underlines (`.cm-lintRange-error`, etc.)
 *   - gutter markers (`.cm-lintPoint-*`)
 *   - hover tooltips with the diagnostic message
 *
 * Lookup model:
 *   1. Read `editorPath` + `activeProject` from `useCortexStore.getState()`
 *      so we don't have to thread props through the extension.
 *   2. Filter diagnostics by basename match — the backend returns
 *      project-relative paths, and we just need "is this for the open file?"
 *   3. Convert 1-based line numbers into byte offsets by walking the doc's
 *      `Line` index (CodeMirror's native, O(log n) line lookup).
 *
 * The extension is no-op safe when nothing is open — the source short-circuits
 * to `[]` if `editorPath` or `activeProject.root` is missing.
 */
import { linter, type Diagnostic as CmDiagnostic } from "@codemirror/lint";
import type { Extension } from "@codemirror/state";

import { projectDiagnostics, type Diagnostic } from "@/lib/context";
import { useCortexStore } from "@/state/store";

/** Idle window before we hit the backend. Matches VS Code's default debounce. */
const LINT_DELAY_MS = 1500;

/** Map backend severity strings → CodeMirror's three-level severity. */
function mapSeverity(s: string): CmDiagnostic["severity"] {
  const lower = s.toLowerCase();
  if (lower === "error") return "error";
  if (lower === "warning" || lower === "warn") return "warning";
  return "info";
}

/** Basename helper — handles both POSIX and Windows separators. */
function basename(path: string): string {
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] ?? path;
}

/**
 * Translate a 1-based line number to a `{from, to}` range covering that
 * full line. Clamps to `[1, doc.lines]` so an out-of-range diagnostic
 * (e.g. a "file deleted" warning pointing past EOF) still renders sanely
 * on the last line instead of throwing.
 */
function lineRange(
  doc: { lines: number; line: (n: number) => { from: number; to: number } },
  line1: number,
): { from: number; to: number } {
  const clamped = Math.max(1, Math.min(line1, doc.lines));
  const l = doc.line(clamped);
  return { from: l.from, to: l.to };
}

export interface LintExtensionOptions {
  /** Override the idle debounce (ms). Defaults to {@link LINT_DELAY_MS}. */
  delayMs?: number;
}

/**
 * Build the lint extension. Call once per editor mount and drop the
 * result into the `extensions` array.
 */
export function lintExtension(opts: LintExtensionOptions = {}): Extension {
  const delay = opts.delayMs ?? LINT_DELAY_MS;

  return linter(
    async (view) => {
      const state = useCortexStore.getState();
      const editorPath = state.editorPath;
      const projectRoot = state.activeProject?.root;
      if (!editorPath || !projectRoot) return [];

      let diags: Diagnostic[];
      try {
        diags = await projectDiagnostics(projectRoot);
      } catch {
        // Backend may be offline or the project may not have a recognised
        // toolchain — both are non-fatal; just render no diagnostics.
        return [];
      }

      const openBase = basename(editorPath);
      const doc = view.state.doc;
      const out: CmDiagnostic[] = [];
      for (const d of diags) {
        if (basename(d.path) !== openBase) continue;
        const { from, to } = lineRange(doc, d.line);
        out.push({
          from,
          to,
          severity: mapSeverity(d.severity),
          message: d.source ? `[${d.source}] ${d.message}` : d.message,
        });
      }
      return out;
    },
    { delay },
  );
}
