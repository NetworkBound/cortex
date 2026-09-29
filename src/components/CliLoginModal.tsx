// In-app CLI sign-in terminal — lazy wrapper.
//
// The implementation (CliLoginModalImpl) mounts an xterm.js Terminal, which is
// the same heavy vendor chunk the embedded TerminalPane uses. Settings renders
// this modal only after the user clicks a provider's "Sign in" button, so the
// wrapper defers loading xterm until that moment instead of dragging it into
// the Settings (and thus startup) bundle. The fallback paints the same backdrop
// + empty dialog frame so the modal opens without a flash of shell behind it.

import { lazy, Suspense } from "react";
import type { CliLoginModalProps } from "./CliLoginModalImpl";

const Impl = lazy(() => import("./CliLoginModalImpl"));

export function CliLoginModal(props: CliLoginModalProps) {
  return (
    <Suspense
      fallback={
        <div className="cli-login-backdrop" role="dialog" aria-modal="true">
          <div className="cli-login-modal" aria-busy="true" />
        </div>
      }
    >
      <Impl {...props} />
    </Suspense>
  );
}
