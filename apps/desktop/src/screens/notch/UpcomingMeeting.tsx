import { Icon } from "../../components/Icon";
import { IconButton } from "../../components/IconButton";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import type { CalendarEventView } from "../../generated/CalendarEventView";
import { nextEvent, promptEvent, relativeStart } from "../../lib/calendar";
import { useNow } from "../../lib/hooks";
import { dispatch } from "../../lib/snapshot";
import "./meeting.css";

const start = (e: CalendarEventView) =>
  void dispatch({ type: "start_calendar_session", event_id: e.id });

/** Under the collapsed pill when a meeting is about to start. */
export function MeetingStrip({ snapshot }: { snapshot: AppSnapshot }) {
  const now = useNow(15_000);
  const event = promptEvent(snapshot.calendar);
  if (snapshot.phase !== "idle" || event === null) return null;
  return (
    <section className="meet-strip" aria-label="Meeting starting">
      <div className="meet-text">
        <span className="meet-title" title={event.title}>
          {event.title}
        </span>
        <span className="meet-when">{relativeStart(event.start_ms, now)}</span>
      </div>
      <button type="button" className="btn primary sm" onClick={() => start(event)}>
        Start
      </button>
      <IconButton
        icon="close"
        label="Dismiss"
        onClick={() => void dispatch({ type: "calendar_dismiss_prompt", event_id: event.id })}
      />
    </section>
  );
}

/** In the expanded panel while idle. */
export function NextMeeting({ snapshot }: { snapshot: AppSnapshot }) {
  const now = useNow(15_000);
  const event = nextEvent(snapshot.calendar, now);
  if (snapshot.phase !== "idle" || event === null) return null;
  const isPrompt = snapshot.calendar.prompt_event_id === event.id;
  return (
    <section className={isPrompt ? "meet-card due" : "meet-card"} aria-label="Next meeting">
      <span className="meet-icon" aria-hidden="true">
        <Icon name={event.has_video ? "video" : "calendar"} size={16} />
      </span>
      <div className="meet-text">
        <span className="meet-title" title={event.title}>
          {event.title}
        </span>
        <span className="meet-when">{relativeStart(event.start_ms, now)}</span>
      </div>
      <button type="button" className="btn primary sm" onClick={() => start(event)}>
        Start
      </button>
      {isPrompt ? (
        <IconButton
          icon="close"
          label="Dismiss"
          onClick={() => void dispatch({ type: "calendar_dismiss_prompt", event_id: event.id })}
        />
      ) : null}
    </section>
  );
}
