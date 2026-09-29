/** Compact relative time: "now", "5m", "3h", "2d", "Apr 4". */
export function relTime(ms: number | undefined | null): string {
  if (!ms || !Number.isFinite(ms)) return "";
  const diff = Date.now() - ms;
  if (diff < 0) return "soon";
  const s = Math.floor(diff / 1000);
  if (s < 45) return "now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d`;
  return new Date(ms).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}

/** "in 3h", "in 12m", "tomorrow 09:00". */
export function untilTime(ms: number | undefined | null): string {
  if (!ms || !Number.isFinite(ms)) return "";
  const diff = ms - Date.now();
  if (diff <= 0) return "due";
  const m = Math.round(diff / 60_000);
  if (m < 60) return `in ${m}m`;
  const h = Math.round(m / 60);
  if (h < 24) return `in ${h}h`;
  return new Date(ms).toLocaleString(undefined, {
    weekday: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function clock(ms: number | undefined | null): string {
  if (!ms) return "";
  return new Date(ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function fmtMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m ${s}s`;
}

export function fmtUsd(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "";
  if (v === 0) return "$0";
  if (v < 0.01) return `$${v.toFixed(4)}`;
  return `$${v.toFixed(2)}`;
}

export function fmtTokens(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "";
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(2)}M`;
}

export function pct(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  const p = v <= 1 ? v * 100 : v;
  return `${Math.round(p)}%`;
}

/** Normalise a 0..1 or 0..100 ratio to 0..100. */
export function pct100(v: number | null | undefined): number {
  if (v === null || v === undefined || !Number.isFinite(v)) return 0;
  const p = v <= 1 ? v * 100 : v;
  return Math.max(0, Math.min(100, p));
}

export function shortId(id: string, n = 4): string {
  return id.length > n * 2 + 1 ? `${id.slice(0, n)}…${id.slice(-n)}` : id;
}

export function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

/** Cheap stable id for client-side rows. */
let seq = 0;
export const localId = (prefix = "c") =>
  `${prefix}${Date.now().toString(36)}${++seq}`;
