import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { App } from "@/App";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { recordJsCrash } from "@/lib/observability";
import { restorePrefsAtBoot } from "@/lib/pref-sync";
import { applyCachedCoreTokens } from "@/lib/theme-engine";
import "@/styles/fonts.css";
import "@/styles/global.css";
// NOTE: highlight.js token colors live in global.css on the theme-adaptive
// `--syntax-*` tokens (GitHub-grade in both dark + light). We deliberately do
// NOT import a vendored hljs stylesheet (e.g. github-dark.css) — those hardcode
// dark colors whose compound selectors override our tokens and wash out on the
// light themes.

// Feed uncaught JS errors + unhandled rejections into the crash store so the
// Crash Viewer ("JS errors" side) actually has data — the `record_js_crash`
// command was never called before this. Best-effort and capped so a render
// loop can't flood the table; failures to record are swallowed.
let crashBudget = 25;
function report(kind: "js_error" | "js_unhandled_rejection", reason: unknown) {
  if (crashBudget-- <= 0) return;
  const err = reason instanceof Error ? reason : null;
  const message = err?.message ?? String(reason ?? "unknown error");
  void recordJsCrash(kind, message.slice(0, 2000), err?.stack).catch(() => {});
}
window.addEventListener("error", (e) =>
  report("js_error", e.error ?? e.message),
);
window.addEventListener("unhandledrejection", (e) =>
  report("js_unhandled_rejection", e.reason),
);

function render() {
  // Paint the last-applied theme on the FIRST frame, synchronously, before
  // React mounts. Both theme systems funnel their tokens into this cache (see
  // theme-engine.ts), so this replays whatever the user actually picked —
  // whichever UI set it — with no flash of the default sheet or a divergent
  // legacy palette. Runs after restorePrefsAtBoot so a post-update wipe has
  // already rehydrated the cache key from disk. useThemeBoot still reconciles
  // against the authoritative backend afterwards.
  applyCachedCoreTokens(document.documentElement);

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </React.StrictMode>,
  );

  // The native window ships hidden (`visible: false` in tauri.conf.json) to
  // avoid the blank gray flash WebView2 shows before its first paint. Reveal it
  // once React has committed and the browser has painted at least one frame. A
  // Rust-side watchdog in lib.rs shows the window anyway if this never runs
  // (e.g. a load failure), so the app can't get stuck invisible.
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      void getCurrentWindow()
        .show()
        .catch(() => {});
    }),
  );
}

// Restore disk-persisted prefs into localStorage BEFORE mounting, so a
// post-update webview wipe doesn't reset onboarding/widths/theme/history. The
// await adds one fast IPC roundtrip; failures never block boot.
restorePrefsAtBoot().finally(render);
