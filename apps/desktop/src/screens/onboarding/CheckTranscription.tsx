import { CheckAdvice, CheckResults } from "../../components/CheckResults";
import { Icon } from "../../components/Icon";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { dispatch } from "../../lib/snapshot";
import { StepFooter } from "./StepFooter";

export function CheckTranscription({
  snapshot,
  onBack,
  onContinue,
}: {
  snapshot: AppSnapshot;
  onBack: () => void;
  onContinue: () => void;
}) {
  const check = snapshot.onboarding.check;
  const running = check.status === "running";
  const never = check.status === "not_run";
  return (
    <>
      <CheckResults check={check} />
      <CheckAdvice check={check} onOpenSettings={() => void dispatch({ type: "open_settings" })} />
      <StepFooter onBack={onBack}>
        <button
          type="button"
          className={never ? "btn primary" : "btn"}
          disabled={running}
          onClick={() => void dispatch({ type: "run_transcription_check" })}
        >
          <Icon name="refresh" size={14} />
          {running ? "Running" : never ? "Run check" : "Run again"}
        </button>
        {never ? (
          <button type="button" className="btn" onClick={onContinue}>
            Skip
          </button>
        ) : (
          <button type="button" className="btn primary" disabled={running} onClick={onContinue}>
            Continue
          </button>
        )}
      </StepFooter>
    </>
  );
}
