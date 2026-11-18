import { useRef, useState } from "react";
import { useSnapshot } from "../../lib/snapshot";
import { isTauri } from "../../lib/tauri";
import { CheckTranscription } from "./CheckTranscription";
import { ConsentStep } from "./ConsentStep";
import { DoneStep } from "./DoneStep";
import { ModelsStep } from "./ModelsStep";
import { PermissionsStep } from "./PermissionsStep";
import { Stepper } from "./Stepper";
import "./onboarding.css";

const STEPS = [
  "Recording consent",
  "Permissions",
  "Check transcription",
  "Models",
  "Done",
] as const;

/** Browser preview only: `#/onboarding?step=2` opens a given step for screenshots. */
function previewStep(): number {
  if (isTauri()) return 0;
  const n = Number(new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("step"));
  return Number.isInteger(n) && n >= 0 && n < STEPS.length ? n : 0;
}

export function OnboardingWindow() {
  const snapshot = useSnapshot();
  const [step, setStep] = useState(previewStep);
  const userMoved = useRef(false);

  // Keyboard and screen reader users land on the new step's title, not on a vanished button.
  const go = (to: (s: number) => number) => {
    userMoved.current = true;
    setStep(to);
  };
  const back = () => go((s) => Math.max(0, s - 1));
  const next = () => go((s) => Math.min(STEPS.length - 1, s + 1));

  return (
    <div className="onb">
      <div className="onb-body">
        <Stepper steps={STEPS} current={step} />
        <main className="onb-main">
          <p className="kick">
            Step {step + 1} of {STEPS.length}
          </p>
          <h1
            key={step}
            tabIndex={-1}
            ref={(el) => {
              if (userMoved.current) el?.focus();
            }}
          >
            {STEPS[step]}
          </h1>
          {snapshot === null ? null : step === 0 ? (
            <ConsentStep snapshot={snapshot} onContinue={next} />
          ) : step === 1 ? (
            <PermissionsStep snapshot={snapshot} onBack={back} onContinue={next} />
          ) : step === 2 ? (
            <CheckTranscription snapshot={snapshot} onBack={back} onContinue={next} />
          ) : step === 3 ? (
            <ModelsStep snapshot={snapshot} onBack={back} onContinue={next} />
          ) : (
            <DoneStep snapshot={snapshot} onBack={back} />
          )}
        </main>
      </div>
    </div>
  );
}
