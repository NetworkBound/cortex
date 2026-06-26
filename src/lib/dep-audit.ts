/**
 * Thin TS wrapper around the `audit_deps` Tauri command.
 *
 * Mirrors `src-tauri::commands::dep_audit::{DepAuditReport, Vulnerability,
 * SeveritySummary}` — keep the field set in sync if you change the Rust
 * structs.
 *
 * The backend caps the entry list at 100 and time-boxes each audit tool at
 * 45s. When the matching audit binary isn't installed we throw a string the
 * modal renders verbatim ("npm not installed — …") so users get an
 * actionable next step instead of a generic stack trace.
 */
import { invoke } from "@tauri-apps/api/core";

/** Normalised severity buckets the backend collapses provider strings into. */
export type Severity = "critical" | "high" | "medium" | "low" | "unknown";

/** Single vulnerability entry as returned by any of the supported ecosystems. */
export interface Vulnerability {
  package: string;
  version: string;
  /** Raw severity string from the provider (e.g. "moderate", "Important"). */
  severity: string;
  summary: string;
  /** CVE / RUSTSEC / GHSA / CWE id when the provider includes one. */
  cve: string | null;
  /** Free-form fix hint ("2.31.0", "yes (npm audit fix)", …). */
  fix_available: string | null;
}

/** Per-severity counts powering the modal's top-of-panel pills. */
export interface SeveritySummary {
  critical: number;
  high: number;
  medium: number;
  low: number;
  unknown: number;
}

/** Full audit report returned to the modal. */
export interface DepAuditReport {
  ecosystem: "npm" | "cargo" | "pip" | string;
  vulnerabilities: Vulnerability[];
  summary: SeveritySummary;
  /** True count before the 100-entry cap is applied. */
  total_count: number;
  /** Last few KiB of stdout/stderr — useful for debugging zero-entry reports. */
  raw_output_tail: string;
}

/**
 * Detect the project's ecosystem and run its dependency-vulnerability audit.
 * Throws when no manifest is found OR when the matching audit binary is
 * missing from PATH — both errors are surfaced verbatim to the user.
 */
export async function auditDeps(projectRoot: string): Promise<DepAuditReport> {
  return invoke<DepAuditReport>("audit_deps", {
    projectRoot,
  });
}

/**
 * Bucket a raw severity string into the 5-tier UI scheme — matches the
 * backend `normalize_severity` mapping. Used for tinting row backgrounds
 * without re-doing the case-fold logic in JSX.
 */
export function normalizeSeverity(raw: string): Severity {
  switch (raw.trim().toLowerCase()) {
    case "critical":
      return "critical";
    case "high":
    case "important":
      return "high";
    case "moderate":
    case "medium":
    case "warning":
      return "medium";
    case "low":
    case "info":
    case "informational":
      return "low";
    default:
      return "unknown";
  }
}

/**
 * Build a search URL for the package on its primary registry. Used by the
 * modal's "Open package on GitHub" button — npm/crates.io/pypi each have
 * their own canonical "show me this package" URL; the GitHub repo link
 * lives on those landing pages.
 */
export function packageRegistryUrl(
  ecosystem: string,
  pkg: string,
): string {
  const safe = encodeURIComponent(pkg);
  switch (ecosystem) {
    case "npm":
      return `https://www.npmjs.com/package/${safe}`;
    case "cargo":
      return `https://crates.io/crates/${safe}`;
    case "pip":
      return `https://pypi.org/project/${safe}/`;
    default:
      return `https://github.com/search?q=${safe}&type=repositories`;
  }
}

/**
 * Build a CVE / advisory URL when an id is present. RUSTSEC / GHSA / CVE
 * each have a canonical landing page; everything else falls back to a
 * generic search so the user always lands somewhere useful.
 */
export function advisoryUrl(cve: string): string {
  const id = cve.trim();
  const upper = id.toUpperCase();
  if (upper.startsWith("RUSTSEC")) {
    return `https://rustsec.org/advisories/${id}.html`;
  }
  if (upper.startsWith("GHSA")) {
    return `https://github.com/advisories/${id}`;
  }
  if (upper.startsWith("CVE")) {
    return `https://nvd.nist.gov/vuln/detail/${id}`;
  }
  if (upper.startsWith("CWE")) {
    return `https://cwe.mitre.org/data/definitions/${id.replace(/^CWE-/i, "")}.html`;
  }
  return `https://www.google.com/search?q=${encodeURIComponent(id)}`;
}
