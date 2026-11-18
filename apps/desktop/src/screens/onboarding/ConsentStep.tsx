import { Icon } from "../../components/Icon";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { dispatch } from "../../lib/snapshot";
import { StepFooter } from "./StepFooter";

export function ConsentStep({
  snapshot,
  onContinue,
}: {
  snapshot: AppSnapshot;
  onContinue: () => void;
}) {
  const acknowledged = snapshot.onboarding.consent_acknowledged;
  return (
    <>
      <section className="note" aria-labelledby="consent-title">
        <h2 id="consent-title">Other people are recorded</h2>
        <p>
          Sotto captures the audio of everyone in your meeting and transcribes it on this Mac. The
          notch shows a listening indicator whenever it does.
        </p>
        <p>
          Consent rules for recording differ by country and state. You are responsible for telling
          the other participants and getting their consent where the law requires it.
        </p>
      </section>
      <StepFooter>
        {acknowledged ? (
          <span className="ack">
            <Icon name="check" size={14} />
            Acknowledged
          </span>
        ) : (
          <button
            type="button"
            className="btn"
            onClick={() => void dispatch({ type: "acknowledge_consent" })}
          >
            I understand and accept
          </button>
        )}
        <button type="button" className="btn primary" disabled={!acknowledged} onClick={onContinue}>
          Continue
        </button>
      </StepFooter>
    </>
  );
}
