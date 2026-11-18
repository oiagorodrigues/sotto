//! When the compact notch offers to start a meeting.

use std::collections::HashSet;

use super::events::CalendarEvent;

/// The prompt appears this long before the event starts.
pub const LEAD_MS: i64 = 2 * 60_000;
/// And stays this long after it started, if the event has not ended.
pub const GRACE_MS: i64 = 10 * 60_000;

/// The event to offer at `now_ms`: inside its window, not dismissed, and no session running.
/// With overlapping events the one starting closest to now wins.
#[must_use]
pub fn pick<'a>(
    events: &'a [CalendarEvent],
    now_ms: i64,
    dismissed: &HashSet<String>,
    session_running: bool,
) -> Option<&'a CalendarEvent> {
    if session_running {
        return None;
    }
    events
        .iter()
        .filter(|e| {
            now_ms >= e.start_ms - LEAD_MS
                && now_ms <= e.start_ms + GRACE_MS
                && now_ms < e.end_ms
                && !dismissed.contains(&e.id)
        })
        .min_by_key(|e| (e.start_ms - now_ms).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: i64 = 1_000_000_000;

    fn ev(id: &str, start: i64, end: i64) -> CalendarEvent {
        CalendarEvent {
            id: id.into(),
            title: id.into(),
            start_ms: start,
            end_ms: end,
            has_video: false,
        }
    }

    fn pick_id(
        events: &[CalendarEvent],
        now: i64,
        dismissed: &[&str],
        running: bool,
    ) -> Option<String> {
        let d = dismissed.iter().map(|s| (*s).to_string()).collect();
        pick(events, now, &d, running).map(|e| e.id.clone())
    }

    #[test]
    fn window_opens_two_minutes_before_and_closes_ten_after() {
        let events = [ev("a", START, START + 3_600_000)];
        assert_eq!(pick_id(&events, START - LEAD_MS - 1, &[], false), None);
        assert_eq!(
            pick_id(&events, START - LEAD_MS, &[], false).as_deref(),
            Some("a")
        );
        assert_eq!(pick_id(&events, START, &[], false).as_deref(), Some("a"));
        assert_eq!(
            pick_id(&events, START + GRACE_MS, &[], false).as_deref(),
            Some("a")
        );
        assert_eq!(pick_id(&events, START + GRACE_MS + 1, &[], false), None);
    }

    #[test]
    fn nothing_while_a_session_runs_or_after_dismissal() {
        let events = [ev("a", START, START + 3_600_000)];
        assert_eq!(pick_id(&events, START, &[], true), None);
        assert_eq!(pick_id(&events, START, &["a"], false), None);
        assert_eq!(pick_id(&events, START, &["b"], false).as_deref(), Some("a"));
    }

    #[test]
    fn an_event_that_already_ended_is_not_offered() {
        let events = [ev("short", START, START + 5 * 60_000)];
        assert_eq!(pick_id(&events, START + 6 * 60_000, &[], false), None);
    }

    #[test]
    fn overlapping_events_pick_the_closest_start() {
        let events = [
            ev("early", START - 5 * 60_000, START + 3_600_000),
            ev("late", START + 60_000, START + 3_600_000),
        ];
        assert_eq!(pick_id(&events, START, &[], false).as_deref(), Some("late"));
        assert_eq!(
            pick_id(&events, START, &["late"], false).as_deref(),
            Some("early")
        );
    }
}
