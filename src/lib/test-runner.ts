import { invoke } from "@tauri-apps/api/core";

/**
 * Frontend bridge to the `run_tests` Tauri command.
 *
 * Auto-detects the test framework in `projectRoot` (Cargo / Vitest / Jest /
 * Mocha / Pytest), runs it, and returns a structured summary the
 * TestRunnerPanel renders. Mirrors `TestResult` / `TestFailure` in
 * `src-tauri/src/commands/test_runner.rs` one-for-one.
 */

export interface TestFailure {
  /** Test name (e.g. `tests::it_works`, `MyComponent > renders`). */
  name: string;
  /** File path + optional `:line`, when the framework prints it. */
  location: string | null;
  /** One-line failure message. Empty when no assertion line was found. */
  message: string;
}

export interface TestResult {
  framework: string;
  command: string;
  passed: number;
  failed: number;
  skipped: number;
  duration_ms: number;
  stdout_tail: string;
  stderr_tail: string;
  exit_code: number;
  failures: TestFailure[];
}

/**
 * Run the project's test suite. Pass `framework` to skip auto-detection
 * (`"cargo" | "vitest" | "jest" | "mocha" | "pytest"`). Throws when no
 * framework is detected or the spawn fails.
 */
export async function runTests(
  projectRoot: string,
  framework?: string,
): Promise<TestResult> {
  return invoke<TestResult>("run_tests", {
    projectRoot,
    framework: framework && framework.trim().length > 0 ? framework : null,
  });
}

/**
 * Parse a `location` string of the form `path/to/file.rs:42` into
 * `{ path, line }`. Returns `null` when the format doesn't match so callers
 * can fall back to opening just the file.
 */
export function parseLocation(
  loc: string | null,
): { path: string; line: number | null } | null {
  if (!loc) return null;
  const m = loc.match(/^(.+?):(\d+)$/);
  if (m) return { path: m[1], line: parseInt(m[2], 10) };
  return { path: loc, line: null };
}

/**
 * Pick a one-word status label from a TestResult — handy for badge text and
 * desktop notifications. Mirrors the colour pills the panel renders.
 */
export function statusLabel(r: TestResult | null, running: boolean): string {
  if (running) return "running";
  if (!r) return "idle";
  if (r.failed > 0) return "failed";
  if (r.exit_code !== 0) return "errored";
  return "passed";
}
