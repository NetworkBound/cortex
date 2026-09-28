/**
 * Shared display formatters. Like `time.ts`, this replaces per-module copies
 * that had drifted apart (four `formatBytes`, five `truncate`); keep new
 * number/string formatting helpers here rather than beside their first caller.
 */

/** Human-friendly byte count: `512 B`, `1.5 KB`, `2.00 MB`, `3.00 GB`. */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(2)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Truncate to at most `max` characters, ending in an ellipsis when cut. */
export function truncate(s: string, max = 120): string {
  if (s.length <= max) return s;
  return `${s.slice(0, Math.max(0, max - 1))}…`;
}
