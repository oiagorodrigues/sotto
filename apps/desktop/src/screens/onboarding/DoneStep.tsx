import type { AppSnapshot } from "../../generated/AppSnapshot";
import { dispatch } from "../../lib/snapshot";
import { StepFooter } from "./StepFooter";

export function DoneStep({ snapshot, onBack }: { snapshot: AppSnapshot; onBack: () => void }) {
  const phase = snapshot.phase;
  return (
    <>
      <section className="note" aria-labelledby="done-title">
        <h2 id="done-title">Sotto is ready</h2>
        <p>
          {phase === "onboarding"
            ? "The notch appears at the top of your screen. Click it to expand, or press the listen shortcut to start a session."
            : "Your setup is saved. Close this window to go back to the notch."}
        </p>
      </section>
      <StepFooter onBack={onBack}>
        <button
          type="button"
          className="btn primary"
          onClick={() => void dispatch({ type: "complete_onboarding" })}
        >
          {phase === "onboarding" ? "Finish setup" : "Close"}
        </button>
      </StepFooter>
    </>
  );
}
