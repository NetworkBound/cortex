/**
 * Issue-to-Agent pipeline — data layer (issue 007, MVP).
 *
 * Bridges tracked forge issues (GitHub / GitLab) to the existing lane
 * machinery: read-only import → triage list → "Run in a lane" (worktree-
 * isolated gateway run) → approval-gated PR + progress comment. Forge tokens
 * live in the encrypted KeyVault only (provider "github" / "gitlab"); the
 * backend never returns them across the bridge. NOTHING is pushed or posted
 * without spending a one-shot approval token from `previewIssuePr`.
 */
import { invoke } from "@tauri-apps/api/core";
import type { LaneRunRecord } from "@/lib/multi-provider";

/** Lightweight local triage heuristic (labels first, keyword fallback) —
 *  never a model/agent judgment call. See backend `classify_issue`. */
export type IssueKind = "bug" | "feature" | "chore" | "unknown";

/** One imported issue, normalized across forges (backend `ForgeIssue`). */
export interface ForgeIssue {
  forge: string;
  owner: string;
  repo: string;
  number: number;
  title: string;
  body: string;
  state: string;
  url: string;
  labels: string[];
  author: string;
  updated_at: string;
  kind: IssueKind;
}

/** The slice of an issue the later pipeline stages need (backend `IssueRef`). */
export interface IssueRef {
  forge: string;
  owner: string;
  repo: string;
  number: number;
  title: string;
  body: string;
  url: string;
  /** Self-hosted forge base URL; public hosts when null. */
  base_url: string | null;
}

/** A previewed draft PR + the one-shot token that authorizes (only) it. */
export interface IssuePrDraft {
  approval_token: string;
  run_id: string;
  title: string;
  body: string;
  head_branch: string;
  expires_unix_ms: number;
}

/** Outcome of the approved PR + progress comment (backend `IssuePrResult`). */
export interface IssuePrResult {
  pr_number: number;
  pr_url: string;
  base: string;
  head: string;
  /** The comment on the source issue is best-effort — false means the PR
   *  opened but the comment failed (see `comment_error`). */
  comment_posted: boolean;
  comment_error: string | null;
}

/** Reduce a full imported issue to the ref later stages carry around. */
export function toIssueRef(issue: ForgeIssue, baseUrl?: string): IssueRef {
  return {
    forge: issue.forge,
    owner: issue.owner,
    repo: issue.repo,
    number: issue.number,
    title: issue.title,
    body: issue.body,
    url: issue.url,
    base_url: baseUrl?.trim() ? baseUrl.trim() : null,
  };
}

/**
 * READ-ONLY import of a repo's open issues (one GET, no side effects on the
 * forge). Anonymous for public repos; a KeyVault key under provider
 * "github" / "gitlab" is used automatically when present.
 */
export async function importIssues(
  forge: string,
  owner: string,
  repo: string,
  baseUrl?: string,
): Promise<ForgeIssue[]> {
  return invoke<ForgeIssue[]>("issues_import", {
    args: { forge, owner, repo, base_url: baseUrl?.trim() ? baseUrl.trim() : null },
  });
}

/**
 * Dispatch an issue onto one worktree-isolated lane (existing lane
 * machinery; the run rides the gateway's model routing). Returns the
 * persisted lane row — progress arrives via `lanes:updated`.
 */
export async function runIssueInLane(
  giteaOwner: string,
  giteaRepo: string,
  provider: string,
  issue: IssueRef,
): Promise<LaneRunRecord> {
  return invoke<LaneRunRecord>("issue_run_in_lane", {
    args: { gitea_owner: giteaOwner, gitea_repo: giteaRepo, provider, issue },
  });
}

/**
 * Dry-run the PR for a settled issue lane: NO network I/O, no writes. Renders
 * the draft and mints the one-shot approval token `openIssuePr` spends.
 */
export async function previewIssuePr(runId: string, issue: IssueRef): Promise<IssuePrDraft> {
  return invoke<IssuePrDraft>("issue_pr_preview", { runId, issue });
}

/**
 * THE approval gate: spends the one-shot token, opens the review PR on Gitea
 * and posts a progress comment on the source issue. Invalid/expired/replayed
 * tokens perform no writes.
 */
export async function openIssuePr(approvalToken: string): Promise<IssuePrResult> {
  return invoke<IssuePrResult>("issue_open_pr", { approvalToken });
}
