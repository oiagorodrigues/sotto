import { useEffect } from "react";
import { Icon } from "../../components/Icon";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import type { PermissionKind } from "../../generated/PermissionKind";
import type { PermissionStatus } from "../../generated/PermissionStatus";
import { dispatch } from "../../lib/snapshot";
import { StepFooter } from "./StepFooter";

const POLL_MS = 2_000;

const ROWS: { kind: PermissionKind; name: string; reason: string; optional?: boolean }[] = [
  { kind: "microphone", name: "Microphone", reason: "Hears your side of the call." },
  {
    kind: "system_audio",
    name: "System audio",
    reason: "Captures what the other participants say.",
  },
  {
    kind: "speech_recognition",
    name: "Speech recognition",
    reason: "Transcribes the audio on this Mac.",
  },
  {
    kind: "screen_recording",
    name: "Screen recording",
    reason: "Only needed to attach a screenshot to an ask.",
    optional: true,
  },
];

const STATUS_LABEL: Record<PermissionStatus, string> = {
  granted: "Allowed",
  denied: "Denied",
  not_determined: "Not asked yet",
  restricted: "Restricted",
};

export function PermissionsStep({
  snapshot,
  onBack,
  onContinue,
}: {
  snapshot: AppSnapshot;
  onBack: () => void;
  onContinue: () => void;
}) {
  useEffect(() => {
    const check = () => void dispatch({ type: "check_permissions" });
    check();
    const id = setInterval(check, POLL_MS);
    window.addEventListener("focus", check);
    return () => {
      clearInterval(id);
      window.removeEventListener("focus", check);
    };
  }, []);

  const statusOf = (kind: PermissionKind): PermissionStatus | null =>
    snapshot.onboarding.permissions.find((p) => p.permission === kind)?.status ?? null;
  const required = ROWS.filter((r) => !r.optional);
  const ready = required.every((r) => statusOf(r.kind) === "granted");

  return (
    <>
      <ul className="perms">
        {ROWS.map((row) => {
          const status = statusOf(row.kind);
          return (
            <li key={row.kind} className="perm">
              <span className="perm-t">
                <span className="perm-name">
                  <b id={`perm-${row.kind}`}>{row.name}</b>
                  {row.optional ? <span className="chip">Optional</span> : null}
                </span>
                <span className="perm-reason">{row.reason}</span>
              </span>
              <span className={`perm-s ${status ?? "unknown"}`}>
                {status === "granted" ? (
                  <Icon name="check" size={14} />
                ) : (
                  <span className="led" aria-hidden="true" />
                )}
                {status === null ? "Checking" : STATUS_LABEL[status]}
              </span>
              <PermissionAction kind={row.kind} name={row.name} status={status} />
            </li>
          );
        })}
      </ul>
      <StepFooter onBack={onBack}>
        <button type="button" className="btn primary" onClick={onContinue}>
          {ready ? "Continue" : "Continue anyway"}
        </button>
      </StepFooter>
    </>
  );
}

function PermissionAction({
  kind,
  name,
  status,
}: {
  kind: PermissionKind;
  name: string;
  status: PermissionStatus | null;
}) {
  if (status === "granted" || status === null) return <span />;
  if (status === "not_determined") {
    return (
      <button
        type="button"
        className="btn sm"
        aria-describedby={`perm-${kind}`}
        onClick={() => void dispatch({ type: "request_permission", permission: kind })}
      >
        Allow
      </button>
    );
  }
  return (
    <button
      type="button"
      className="btn sm"
      aria-label={`Open System Settings for ${name}`}
      onClick={() => void dispatch({ type: "open_privacy_settings", permission: kind })}
    >
      Open System Settings
    </button>
  );
}
