//! The core's side of the calendar: forwards intents to the calendar task, keeps the dismissed
//! prompts and turns the task's state into the snapshot's `CalendarView`.

use std::collections::HashSet;

use sotto_protocol::ui::{CalendarEventView, CalendarView};
use tokio::sync::watch;

use crate::calendar::{CalendarHandle, CalendarState, Command, pick_prompt};

const MAX_SESSION_TITLE_CHARS: usize = 120;

#[derive(Debug)]
pub(crate) struct CalendarLink {
    handle: CalendarHandle,
    /// `None` once the task has stopped, so the select branch parks instead of spinning.
    changes: Option<watch::Receiver<CalendarState>>,
    dismissed: HashSet<String>,
}

impl CalendarLink {
    pub fn new(handle: CalendarHandle) -> Self {
        let changes = Some(handle.state());
        Self {
            handle,
            changes,
            dismissed: HashSet::new(),
        }
    }

    pub fn send(&self, command: Command) {
        self.handle.send(command);
    }

    pub fn dismiss(&mut self, event_id: String) {
        self.dismissed.insert(event_id);
    }

    /// Resolves when the task publishes a new state.
    pub async fn changed(&mut self) {
        match &mut self.changes {
            Some(rx) => {
                if rx.changed().await.is_err() {
                    self.changes = None;
                }
            }
            None => std::future::pending().await,
        }
    }

    /// The title to give a session started from this event, if the event is still listed.
    pub fn session_title(&self, event_id: &str) -> Option<String> {
        let state = self.handle.state();
        let state = state.borrow();
        let title = &state.events.iter().find(|e| e.id == event_id)?.title;
        Some(title.chars().take(MAX_SESSION_TITLE_CHARS).collect())
    }

    pub fn view(&self, now_ms: i64, session_running: bool) -> CalendarView {
        let state = self.handle.state();
        let state = state.borrow();
        let prompt = pick_prompt(&state.events, now_ms, &self.dismissed, session_running)
            .map(|e| e.id.clone());
        CalendarView {
            connection: state.connection,
            error: state.error.clone(),
            masked_client_id: state.masked_client_id.clone(),
            has_client_secret: state.has_client_secret,
            events: state
                .events
                .iter()
                .map(|e| CalendarEventView {
                    id: e.id.clone(),
                    title: e.title.clone(),
                    start_ms: e.start_ms,
                    end_ms: e.end_ms,
                    has_video: e.has_video,
                })
                .collect(),
            prompt_event_id: prompt,
        }
    }
}

#[cfg(test)]
mod tests {
    use sotto_protocol::ui::CalendarConnection;

    use super::*;
    use crate::calendar::{CalendarEvent, GRACE_MS, LEAD_MS};

    const START: i64 = 5_000_000_000;

    fn state(events: Vec<CalendarEvent>) -> CalendarState {
        CalendarState {
            connection: CalendarConnection::Connected,
            events,
            ..CalendarState::not_configured()
        }
    }

    fn event(id: &str, title: &str) -> CalendarEvent {
        CalendarEvent {
            id: id.into(),
            title: title.into(),
            start_ms: START,
            end_ms: START + 3_600_000,
            has_video: true,
        }
    }

    #[test]
    fn the_view_offers_the_prompt_inside_its_window_only() {
        let (handle, tx, _rx) = CalendarHandle::for_test();
        tx.send_replace(state(vec![event("a", "Planning")]));
        let link = CalendarLink::new(handle);
        assert_eq!(link.view(START - LEAD_MS - 1, false).prompt_event_id, None);
        let view = link.view(START - LEAD_MS, false);
        assert_eq!(view.prompt_event_id.as_deref(), Some("a"));
        assert_eq!(view.events.len(), 1);
        assert_eq!(view.events[0].title, "Planning");
        assert_eq!(link.view(START + GRACE_MS + 1, false).prompt_event_id, None);
    }

    #[test]
    fn a_running_session_or_a_dismissal_hides_the_prompt() {
        let (handle, tx, _rx) = CalendarHandle::for_test();
        tx.send_replace(state(vec![event("a", "Planning"), event("b", "Other")]));
        let mut link = CalendarLink::new(handle);
        assert_eq!(link.view(START, true).prompt_event_id, None);
        link.dismiss("a".into());
        assert_eq!(
            link.view(START, false).prompt_event_id.as_deref(),
            Some("b")
        );
        link.dismiss("b".into());
        assert_eq!(link.view(START, false).prompt_event_id, None);
    }

    #[test]
    fn session_titles_come_from_the_core_s_own_event_list() {
        let (handle, tx, _rx) = CalendarHandle::for_test();
        tx.send_replace(state(vec![event("a", &"T".repeat(300))]));
        let link = CalendarLink::new(handle);
        assert_eq!(
            link.session_title("a").unwrap().chars().count(),
            MAX_SESSION_TITLE_CHARS
        );
        assert_eq!(link.session_title("missing"), None);
    }

    #[tokio::test]
    async fn changed_resolves_when_the_state_changes() {
        let (handle, tx, _rx) = CalendarHandle::for_test();
        let mut link = CalendarLink::new(handle);
        tx.send_replace(state(Vec::new()));
        tokio::time::timeout(std::time::Duration::from_secs(1), link.changed())
            .await
            .unwrap();
    }
}
