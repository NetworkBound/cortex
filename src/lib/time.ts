// Shared relative-time formatting. Previously this exact logic was copy-pasted
// across ~21 components and lib modules (each a slightly drifted variant); this
// is the single source of truth. Per-domain differences (empty label, absolute
// fallback for old items, coarse "just now"/"mo" buckets) are expressed via
// options rather than forked copies.

export interface TimeAgoOptions {
  /** Label rendered when `ts` is falsy (0 / null / undefined). Default "—". */
  empty?: string;
  /**
   * Once the delta exceeds this many days, render an absolute locale date
   * (`toLocaleDateString()`) instead of "Nd ago". Default: never (always
   * relative). Used by long-lived logs (audit, notifications, crashes).
   */
  absoluteAfterDays?: number;
  /**
   * Coarse buckets: render "just now" under a minute (no seconds) and "Nmo ago"
   * past 30 days. Used where second-level precision is noise (workspace age).
   */
  coarse?: boolean;
}

/**
 * Human "N{s,m,h,d} ago" for a unix-ms timestamp. Future timestamps and the
 * sub-second present both read "just now".
 */
export function timeAgo(
  ts: number | null | undefined,
  opts: TimeAgoOptions = {},
): string {
  const empty = opts.empty ?? "—";
  if (!ts) return empty;

  const s = Math.floor((Date.now() - ts) / 1000);
  if (s < 0) return "just now";
  if (opts.coarse && s < 60) return "just now";
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;

  const days = Math.floor(s / 86400);
  if (opts.absoluteAfterDays != null && days >= opts.absoluteAfterDays) {
    return new Date(ts).toLocaleDateString();
  }
  if (opts.coarse && days >= 30) return `${Math.floor(days / 30)}mo ago`;
  return `${days}d ago`;
}
