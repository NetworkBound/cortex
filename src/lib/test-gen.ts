/**
 * Thin TS wrapper around the `generate_tests` Tauri command.
 *
 * Mirrors `src-tauri::commands::test_gen::TestGenResult`. The backend caps
 * the source blob at 64 KiB and the gateway call at 45s — callers should
 * surface the error message returned from the promise rejection.
 */
import { invoke } from "@tauri-apps/api/core";

/** Canonical framework keys understood by the backend `resolve_framework`. */
export type TestFramework =
  | "auto"
  | "cargo"
  | "vitest"
  | "jest"
  | "mocha"
  | "pytest";

/**
 * Mirrors `src-tauri::commands::test_gen::TestGenResult`. `framework` is the
 * post-resolution canonical key (never the raw user input — the backend
 * folds "auto" / empty into the language default).
 */
export interface TestGenResult {
  path: string;
  function_name: string | null;
  language: string;
  framework: string;
  test_code: string;
  /** Absolute path the frontend should pre-fill the "Save to suggested
   *  path" action with. Backend computes from language + framework. */
  suggested_test_path: string;
  generated_unix_ms: number;
}

/**
 * Ask the gateway to generate unit tests for the given file. Pass
 * `functionName` to scope the tests to a single function; otherwise the
 * whole file is fed in. Pass `framework` to force a specific framework;
 * omit (or pass "auto") to let the backend pick by language + package.json.
 */
export async function generateTests(
  path: string,
  functionName?: string | null,
  framework?: TestFramework,
): Promise<TestGenResult> {
  const cleanedFn =
    functionName && functionName.trim().length > 0 ? functionName.trim() : null;
  const cleanedFw =
    framework && framework !== "auto" ? framework : null;
  return invoke<TestGenResult>("generate_tests", {
    path,
    functionName: cleanedFn,
    framework: cleanedFw,
  });
}
