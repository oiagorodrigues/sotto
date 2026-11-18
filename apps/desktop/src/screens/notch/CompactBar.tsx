import { Icon } from "../../components/Icon";
import { IconButton } from "../../components/IconButton";
import { Tooltip } from "../../components/Tooltip";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { formatTimer } from "../../lib/format";
import { useNow } from "../../lib/hooks";
import { dispatch } from "../../lib/snapshot";
import { EngineMark } from "./EngineMark";
import { Bars, Dots } from "./indicators";
import { deriveStatus, LEVEL_STEPS, type Status } from "./status";

interface Props {
  snapshot: AppSnapshot;
  expanded: boolean;
  onExpand: () => void;
  onCollapse: () => void;
}

export function isListening(snapshot: AppSnapshot): boolean {
  return snapshot.phase === "listening";
}

export function toggleListening(snapshot: AppSnapshot): Promise<void> {
  return isListening(snapshot)
    ? dispatch({ type: "end_session" })
    : dispatch({ type: "start_session", mode_id: null });
}

/** The row at notch height: the pill when collapsed, the top of the panel when expanded. */
export function CompactBar({ snapshot, expanded, onExpand, onCollapse }: Props) {
  const status = deriveStatus(snapshot);
  const listening = isListening(snapshot);
  return (
    <div className="bar">
      {expanded ? null : (
        <button
          type="button"
          className="hit"
          aria-label="Expand"
          aria-expanded={false}
          onClick={onExpand}
        />
      )}
      <div className="wing">
        <MicButton snapshot={snapshot} status={status} expanded={expanded} />
        {expanded && status.kind !== "idle" ? (
          <span className={listening ? "wing-state live" : "wing-state"}>{status.label}</span>
        ) : null}
      </div>
      <span className="gap" aria-hidden="true" />
      <div className="wing end">
        {snapshot.invisible && !expanded ? (
          <span
            className="inv"
            role="img"
            aria-label="Hidden from screen shares"
            title="Hidden from screen shares"
          >
            <Icon name="eye-off" size={12} />
          </span>
        ) : null}
        {expanded ? null : <AnswerGlyph status={status} />}
        {listening && snapshot.engine === "cloud" ? <EngineMark expanded={expanded} /> : null}
        {listening ? <Timer startedAt={snapshot.session?.started_at_unix_ms ?? 0} /> : null}
        {expanded ? (
          <IconButton
            icon="chevron"
            label="Collapse"
            shortcut="⌘\"
            className="icon-btn collapse"
            onClick={onCollapse}
          />
        ) : null}
      </div>
    </div>
  );
}

function Timer({ startedAt }: { startedAt: number }) {
  const now = useNow(1000);
  return <time className="mono timer">{formatTimer(now - startedAt)}</time>;
}

function MicButton({
  snapshot,
  status,
  expanded,
}: {
  snapshot: AppSnapshot;
  status: Status;
  expanded: boolean;
}) {
  const listening = isListening(snapshot);
  const disabled = snapshot.phase === "onboarding";
  const level = Math.max(snapshot.levels.me, snapshot.levels.them);
  const label = listening ? "Stop listening" : "Start listening";
  const button = (
    <button
      type="button"
      className={listening ? "mic on" : "mic"}
      aria-label={label}
      aria-pressed={listening}
      data-status={status.kind}
      disabled={disabled}
      title={expanded ? undefined : `${label} (⌘⇧L)`}
      onClick={() => void toggleListening(snapshot)}
    >
      {listening ? <Bars level={level} steps={LEVEL_STEPS} /> : <Icon name="mic" size={16} />}
    </button>
  );
  return expanded ? (
    <Tooltip label={label} shortcut="⌘⇧L">
      {button}
    </Tooltip>
  ) : (
    button
  );
}

export function AnswerGlyph({ status }: { status: Status }) {
  switch (status.kind) {
    case "preparing":
      return (
        <output className="glyph prep" title={status.label} aria-label={status.label}>
          <Dots />
        </output>
      );
    case "ready":
      return (
        <output className="glyph ready" title={status.label} aria-label={status.label}>
          <span className="cue-dot" aria-hidden="true" />
        </output>
      );
    case "error":
      return (
        <output className="glyph err" title={status.label} aria-label={status.label}>
          <Icon name="alert" size={13} />
        </output>
      );
    default:
      return null;
  }
}
