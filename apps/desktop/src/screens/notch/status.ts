import type { AppSnapshot } from "../../generated/AppSnapshot";

export type StatusKind = "idle" | "listening" | "them" | "preparing" | "ready" | "error";

/** One word per state, used by the pill, the bar row, the answer card, tooltips and aria-labels. */
export const STATE_LABEL: Record<StatusKind, string> = {
  idle: "Idle",
  listening: "Listening",
  them: "Them speaking",
  preparing: "Preparing answer",
  ready: "Answer ready",
  error: "Error",
};

/** Top of the quantized input level scale; mirrors `LEVEL_STEPS` in `sotto-protocol`. */
export const LEVEL_STEPS = 5;

export interface Status {
  kind: StatusKind;
  label: string;
}

/** Each state has its own word and its own shape, so color is never the only signal. */
export function deriveStatus(s: AppSnapshot): Status {
  const a = s.answer;
  if (a?.state === "failed") return { kind: "error", label: STATE_LABEL.error };
  if (a?.state === "preparing") return { kind: "preparing", label: STATE_LABEL.preparing };
  if (a && (a.state === "streaming" || a.state === "done")) {
    return { kind: "ready", label: STATE_LABEL.ready };
  }
  if (s.phase === "idle" || s.phase === "onboarding")
    return { kind: "idle", label: STATE_LABEL.idle };
  const last = s.transcript[s.transcript.length - 1];
  if (last && last.speaker === "them" && !last.is_final) {
    return { kind: "them", label: STATE_LABEL.them };
  }
  return { kind: "listening", label: STATE_LABEL.listening };
}
