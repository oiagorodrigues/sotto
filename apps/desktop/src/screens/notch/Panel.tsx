import { Icon } from "../../components/Icon";
import { IconButton } from "../../components/IconButton";
import { NoticeBar } from "../../components/NoticeBar";
import { Tooltip } from "../../components/Tooltip";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { keysText, SHORTCUTS } from "../../lib/shortcuts";
import { dispatch } from "../../lib/snapshot";
import { AnswerCard } from "./AnswerCard";
import { AskBar } from "./AskBar";
import { isListening } from "./CompactBar";
import { EmptyState } from "./EmptyState";
import { ModePicker } from "./ModePicker";
import { ShortcutsSheet } from "./ShortcutsSheet";
import { Timeline } from "./Timeline";
import { Transcript } from "./Transcript";
import { NextMeeting } from "./UpcomingMeeting";

interface Props {
  snapshot: AppSnapshot;
  askTick: number;
  sheetOpen: boolean;
  onSheet: (open: boolean) => void;
}

const HINTS = SHORTCUTS.filter((s) => s.id !== "esc");

export function Panel({ snapshot, askTick, sheetOpen, onSheet }: Props) {
  const { session, invisible, answer } = snapshot;
  const listening = isListening(snapshot);
  const started = session?.started_at_unix_ms ?? 0;
  const showEmpty = !listening && (!answer || answer.state === "cancelled");

  return (
    <div className="panel">
      <header className="head">
        <h1 className={session ? "title" : "title muted"}>{session?.title ?? "Not listening"}</h1>
        <ModePicker snapshot={snapshot} listening={listening} />
        <span className="grow" />
        <Tooltip
          label={invisible ? "Hidden from screen shares" : "Visible to screen shares"}
          shortcut="⌘⇧I"
        >
          <button
            type="button"
            className={invisible ? "chip inv" : "chip"}
            aria-pressed={invisible}
            onClick={() => dispatch({ type: "set_invisible", invisible: !invisible })}
          >
            <Icon name={invisible ? "eye-off" : "eye"} size={12} />
            {invisible ? "Invisible" : "Visible"}
          </button>
        </Tooltip>
        <IconButton
          icon="gear"
          label="Settings"
          onClick={() => dispatch({ type: "open_settings" })}
        />
      </header>
      <div className="scroll">
        {sheetOpen ? (
          <ShortcutsSheet onClose={() => onSheet(false)} />
        ) : (
          <>
            {listening ? <Timeline lines={snapshot.transcript} startedAtUnixMs={started} /> : null}
            {showEmpty ? <NextMeeting snapshot={snapshot} /> : null}
            {showEmpty ? <EmptyState snapshot={snapshot} /> : <AnswerCard answer={answer} />}
            {listening || snapshot.transcript.length > 0 ? (
              <Transcript lines={snapshot.transcript} startedAtUnixMs={started} />
            ) : null}
          </>
        )}
        <NoticeBar notice={snapshot.notice} />
      </div>
      <AskBar snapshot={snapshot} askTick={askTick} />
      <footer className="keys">
        <ul aria-label="Shortcuts">
          {HINTS.map((s) => (
            <li key={s.id}>
              <kbd>{keysText(s)}</kbd>
              {s.hint}
            </li>
          ))}
        </ul>
        <IconButton
          icon="help"
          label="Keyboard shortcuts"
          shortcut="?"
          aria-pressed={sheetOpen}
          onClick={() => onSheet(!sheetOpen)}
        />
      </footer>
    </div>
  );
}
