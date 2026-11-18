import { CheckAdvice, CheckResults } from "../../components/CheckResults";
import { Icon } from "../../components/Icon";
import { dispatch, useSnapshot } from "../../lib/snapshot";

export function TranscriptionCheckModule() {
  const snapshot = useSnapshot();
  if (snapshot === null) return null;
  const check = snapshot.onboarding.check;
  const running = check.status === "running";
  return (
    <div className="check-mod">
      <CheckResults check={check} />
      <CheckAdvice check={check} />
      <div className="row">
        <button
          type="button"
          className="btn"
          disabled={running}
          onClick={() => void dispatch({ type: "run_transcription_check" })}
        >
          <Icon name="refresh" size={14} />
          {running ? "Running" : check.status === "not_run" ? "Run check" : "Run again"}
        </button>
      </div>
    </div>
  );
}
