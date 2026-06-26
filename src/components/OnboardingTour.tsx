import { useEffect, useState } from "react";
import {
  consumeForceShow,
  onTourTrigger,
  TOUR_STEPS,
} from "@/lib/onboarding";
import { useCortexStore } from "@/state/store";

/**
 * Lightweight pinned-card tour. Walks the user through five feature
 * highlights (defined in `lib/onboarding.ts`). The tour is gated on the
 * store's `onboardingComplete` flag — if true and the user has not opted
 * back in via `/tour`, this component renders nothing.
 *
 * The card is rendered as a `position: fixed` overlay in the bottom-right
 * corner of the viewport so it doesn't disturb the rest of the layout.
 */
export function OnboardingTour() {
  const onboardingComplete = useCortexStore((s) => s.onboardingComplete);
  const setOnboardingComplete = useCortexStore((s) => s.setOnboardingComplete);
  const markFeatureSeen = useCortexStore((s) => s.markFeatureSeen);

  // Local visibility flag — initialised from the store's first-run gate, but
  // can also be forced true by the `/tour` slash command without flipping the
  // global `onboardingComplete` flag (so power users can replay without
  // resetting the wizard).
  const [active, setActive] = useState<boolean>(() => {
    if (!onboardingComplete) return true;
    return consumeForceShow();
  });
  const [step, setStep] = useState(0);

  // Subscribe to `/tour` triggers. The unsubscribe closure makes this safe
  // for the strict-mode double-mount in dev.
  useEffect(() => {
    const off = onTourTrigger(() => {
      setStep(0);
      setActive(true);
    });
    return off;
  }, []);

  // Also re-evaluate on `onboardingComplete` flipping — if the wizard
  // finishes while this component is mounted we should not immediately fire
  // the tour over the top of it. The wizard already saw the user; the tour
  // becomes opt-in via `/tour` from that point on.
  useEffect(() => {
    if (onboardingComplete) setActive(false);
  }, [onboardingComplete]);

  if (!active) return null;
  if (TOUR_STEPS.length === 0) return null;

  const current = TOUR_STEPS[step] ?? TOUR_STEPS[0];
  const isLast = step >= TOUR_STEPS.length - 1;

  function finish() {
    markFeatureSeen("onboarding-tour");
    setOnboardingComplete(true);
    setActive(false);
  }

  function skip() {
    markFeatureSeen("onboarding-tour");
    setOnboardingComplete(true);
    setActive(false);
  }

  function next() {
    if (isLast) {
      finish();
      return;
    }
    setStep((s) => Math.min(s + 1, TOUR_STEPS.length - 1));
  }

  return (
    <div className="tour-overlay" role="dialog" aria-label="Cortex feature tour">
      <div className="tour-card">
        <div className="tour-card-head">
          <span className="tour-card-step">
            Step {step + 1} of {TOUR_STEPS.length}
          </span>
          <button
            type="button"
            className="tour-card-close"
            onClick={skip}
            aria-label="Skip tour"
            title="Skip"
          >
            ×
          </button>
        </div>
        <h3 className="tour-card-title">{current.title}</h3>
        <p className="tour-card-body">{current.body}</p>
        {current.hint && (
          <div className="tour-card-hint">
            <kbd>{current.hint}</kbd>
          </div>
        )}
        <div className="tour-card-dots">
          {TOUR_STEPS.map((_, i) => (
            <span
              key={i}
              className={`tour-dot${i === step ? " active" : ""}`}
              aria-hidden="true"
            />
          ))}
        </div>
        <div className="tour-card-actions">
          <button type="button" className="tour-btn-skip" onClick={skip}>
            Skip
          </button>
          <button type="button" className="tour-btn-next" onClick={next}>
            {isLast ? "Done" : "Next"}
          </button>
        </div>
      </div>
    </div>
  );
}
