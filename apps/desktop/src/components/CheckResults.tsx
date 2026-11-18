import type { ClipResultView } from "../generated/ClipResultView";
import type { TranscriptionCheckView } from "../generated/TranscriptionCheckView";
import { Icon } from "./Icon";

const SCALE = { first: 2_000, final: 1_500, error: 30 };

export function localeName(tag: string): string {
  try {
    const name = new Intl.DisplayNames(["en"], { type: "language" }).of(tag);
    return name ? `${name} (${tag})` : tag;
  } catch {
    return tag;
  }
}

function notMeasured(r: ClipResultView): string {
  if (r.outcome === "locale_not_installed") return "The on-device model is not installed";
  if (r.outcome === "no_sample") return "No sample clip for this language";
  return r.message ?? "The engine did not produce text";
}

function checkedOn(check: TranscriptionCheckView): string | null {
  if (check.checked_at_unix_ms === null) return null;
  const date = new Date(check.checked_at_unix_ms).toLocaleDateString("en", {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
  return check.macos_version ? `macOS ${check.macos_version} · ${date}` : date;
}

export function CheckResults({ check }: { check: TranscriptionCheckView }) {
  const running = check.status === "running";
  const measured = check.results.filter((r) => r.outcome === "measured");
  const best = (pick: (r: ClipResultView) => number | null) => {
    const values = measured.map(pick).filter((v): v is number => v !== null);
    return values.length > 1 ? Math.min(...values) : null;
  };
  const bestFirst = best((r) => r.first_partial_ms);
  const bestFinal = best((r) => r.final_ms);
  const bestError = best((r) => r.error_rate_pct);
  const when = checkedOn(check);

  return (
    <section className="cmp" aria-labelledby="apple-speech">
      <div className="cmp-h">
        <Icon name="cpu" />
        <h2 id="apple-speech">Apple Speech</h2>
        <span className="chip">On device</span>
        {check.stale && !running ? <span className="chip warn">Out of date</span> : null}
      </div>
      <div className="cmp-cols" aria-hidden="true">
        <span>Language</span>
        <span>First words</span>
        <span>Final text</span>
        <span>Error rate</span>
      </div>
      <div className="cmp-rows" aria-busy={running}>
        {running ? (
          <p className="cmp-status" role="status">
            <span className="spin" aria-hidden="true" />
            Playing the {check.running_locale ? localeName(check.running_locale) : ""} sample, clip{" "}
            <span className="mono">
              {Math.min(check.clips_done + 1, check.clips_total)} of {check.clips_total}
            </span>
          </p>
        ) : null}
        {!running && check.status === "not_run" ? <p className="cmp-status">Not run yet</p> : null}
        {check.results.map((r) =>
          r.outcome === "measured" ? (
            <div className="cmp-row" key={r.locale}>
              <span className="lang">{localeName(r.locale)}</span>
              <Metric
                label="First words"
                value={r.first_partial_ms}
                unit="ms"
                max={SCALE.first}
                best={r.first_partial_ms === bestFirst}
              />
              <Metric
                label="Final text"
                value={r.final_ms}
                unit="ms"
                max={SCALE.final}
                best={r.final_ms === bestFinal}
              />
              <Metric
                label="Error rate"
                value={r.error_rate_pct === null ? null : Math.round(r.error_rate_pct * 10) / 10}
                unit="%"
                max={SCALE.error}
                best={r.error_rate_pct === bestError}
              />
            </div>
          ) : (
            <div className="cmp-row failed" key={r.locale}>
              <span className="lang">{localeName(r.locale)}</span>
              <p className="cmp-fail">
                <Icon name="alert" size={14} />
                {notMeasured(r)}
              </p>
            </div>
          ),
        )}
      </div>
      {when && !running ? <p className="cmp-meta mono">{when}</p> : null}
    </section>
  );
}

function Metric({
  label,
  value,
  unit,
  max,
  best,
}: {
  label: string;
  value: number | null;
  unit: string;
  max: number;
  best: boolean;
}) {
  return (
    <div className={best ? "m best" : "m"}>
      <span className="k">{label}</span>
      <span className="v">
        {value ?? "–"}
        <small>{value === null ? "" : unit}</small>
      </span>
      <span className="bar-bg" aria-hidden="true">
        <i style={{ width: `${Math.min(100, ((value ?? 0) / max) * 100)}%` }} />
      </span>
    </div>
  );
}

/** What to do when the on-device engine could not be measured. */
export function CheckAdvice({ check }: { check: TranscriptionCheckView }) {
  const problem = check.status === "failed" || check.results.some((r) => r.outcome !== "measured");
  if (check.status === "running" || !problem) return null;
  return (
    <p className="cmp-advice" role="status">
      <Icon name="info" size={14} />
      {check.error ? `${check.error}. ` : ""}Cloud transcription can be turned on in Settings &gt;
      Transcription.
    </p>
  );
}
