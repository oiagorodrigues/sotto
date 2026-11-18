//! Google Calendar `events.list` for the primary calendar: request and parsing.

use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::error::CalendarError;

pub const HORIZON_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_RESULTS: u32 = 100;
const MAX_EVENTS: usize = 50;
const MAX_TITLE_CHARS: usize = 200;
const NO_TITLE: &str = "(No title)";
/// Only the fields the app uses leave Google.
const FIELDS: &str = "items(id,status,summary,start,end,hangoutLink,conferenceData(entryPoints(entryPointType)),attendees(self,responseStatus))";

#[derive(Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub has_video: bool,
}

impl std::fmt::Debug for CalendarEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalendarEvent")
            .field("start_ms", &self.start_ms)
            .field("end_ms", &self.end_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    items: Vec<RawEvent>,
}

#[derive(Deserialize)]
struct RawEvent {
    id: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    start: Option<RawTime>,
    end: Option<RawTime>,
    #[serde(rename = "hangoutLink")]
    hangout_link: Option<String>,
    #[serde(rename = "conferenceData")]
    conference_data: Option<RawConference>,
    #[serde(default)]
    attendees: Vec<RawAttendee>,
}

#[derive(Deserialize)]
struct RawTime {
    #[serde(rename = "dateTime")]
    date_time: Option<String>,
}

#[derive(Deserialize)]
struct RawConference {
    #[serde(default, rename = "entryPoints")]
    entry_points: Vec<RawEntryPoint>,
}

#[derive(Deserialize)]
struct RawEntryPoint {
    #[serde(rename = "entryPointType")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct RawAttendee {
    #[serde(default, rename = "self")]
    is_self: bool,
    #[serde(rename = "responseStatus")]
    response: Option<String>,
}

fn parse_time(t: Option<&RawTime>) -> Option<i64> {
    let text = t?.date_time.as_deref()?;
    let parsed = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).ok()
}

/// Skips cancelled events, all-day events (they carry a `date`, not a `dateTime`, so they are
/// not meetings with a start time), events the user declined and events that cannot be read.
pub fn parse_events(body: &[u8]) -> Result<Vec<CalendarEvent>, CalendarError> {
    let page: Page = serde_json::from_slice(body).map_err(|_| CalendarError::BadResponse)?;
    let mut events: Vec<CalendarEvent> = page
        .items
        .into_iter()
        .filter_map(|raw| {
            if raw.status.as_deref() == Some("cancelled") {
                return None;
            }
            if raw
                .attendees
                .iter()
                .any(|a| a.is_self && a.response.as_deref() == Some("declined"))
            {
                return None;
            }
            let id = raw.id.filter(|i| !i.is_empty())?;
            let start_ms = parse_time(raw.start.as_ref())?;
            let end_ms = parse_time(raw.end.as_ref())?;
            if end_ms < start_ms {
                return None;
            }
            let has_video = raw.hangout_link.is_some()
                || raw.conference_data.is_some_and(|c| {
                    c.entry_points
                        .iter()
                        .any(|p| p.kind.as_deref() == Some("video"))
                });
            let title = raw
                .summary
                .map(|s| s.trim().chars().take(MAX_TITLE_CHARS).collect::<String>())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| NO_TITLE.to_string());
            Some(CalendarEvent {
                id,
                title,
                start_ms,
                end_ms,
                has_video,
            })
        })
        .collect();
    events.sort_by_key(|e| (e.start_ms, e.end_ms));
    events.truncate(MAX_EVENTS);
    Ok(events)
}

fn rfc3339(ms: i64) -> Result<String, CalendarError> {
    OffsetDateTime::from_unix_timestamp(ms.div_euclid(1000))
        .map_err(|_| CalendarError::BadResponse)?
        .format(&Rfc3339)
        .map_err(|_| CalendarError::BadResponse)
}

#[derive(Debug)]
pub enum ListError {
    /// The access token was refused.
    Unauthorized,
    Other(CalendarError),
}

pub async fn list_events(
    http: &reqwest::Client,
    api_base: &str,
    access_token: &str,
    now_ms: i64,
) -> Result<Vec<CalendarEvent>, ListError> {
    let url = format!(
        "{}/calendars/primary/events",
        api_base.trim_end_matches('/')
    );
    let time_min = rfc3339(now_ms).map_err(ListError::Other)?;
    let time_max = rfc3339(now_ms + HORIZON_MS).map_err(ListError::Other)?;
    let response = http
        .get(url)
        .bearer_auth(access_token)
        .query(&[
            ("singleEvents", "true"),
            ("orderBy", "startTime"),
            ("timeMin", time_min.as_str()),
            ("timeMax", time_max.as_str()),
            ("maxResults", &MAX_RESULTS.to_string()),
            ("fields", FIELDS),
        ])
        .send()
        .await
        .map_err(|e| ListError::Other(CalendarError::from_http(&e)))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ListError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ListError::Other(CalendarError::Rejected(format!(
            "calendar request returned {}",
            status.as_u16()
        ))));
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| ListError::Other(CalendarError::from_http(&e)))?;
    parse_events(&body).map_err(ListError::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Vec<CalendarEvent> {
        parse_events(json.as_bytes()).unwrap()
    }

    #[test]
    fn parses_offsets_and_orders_by_start() {
        let events = parse(
            r#"{"items":[
            {"id":"b","summary":"Later","start":{"dateTime":"2026-11-06T15:00:00+09:00"},"end":{"dateTime":"2026-11-06T16:00:00+09:00"}},
            {"id":"a","summary":"Earlier","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T10:30:00Z"}}
            ]}"#,
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "b");
        assert_eq!(events[0].start_ms, 1_793_944_800_000);
        assert_eq!(events[0].end_ms - events[0].start_ms, 3_600_000);
        assert_eq!(events[1].start_ms, 1_793_959_200_000);
    }

    #[test]
    fn same_instant_in_two_zones_is_equal() {
        let events = parse(
            r#"{"items":[
            {"id":"a","start":{"dateTime":"2026-11-06T12:00:00-03:00"},"end":{"dateTime":"2026-11-06T13:00:00-03:00"}},
            {"id":"b","start":{"dateTime":"2026-11-06T15:00:00Z"},"end":{"dateTime":"2026-11-06T16:00:00Z"}}
            ]}"#,
        );
        assert_eq!(events[0].start_ms, events[1].start_ms);
    }

    #[test]
    fn skips_all_day_cancelled_declined_and_unreadable() {
        let events = parse(
            r#"{"items":[
            {"id":"allday","summary":"Holiday","start":{"date":"2026-11-06"},"end":{"date":"2026-11-07"}},
            {"id":"cancelled","status":"cancelled"},
            {"id":"declined","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"},
             "attendees":[{"self":true,"responseStatus":"declined"},{"responseStatus":"accepted"}]},
            {"id":"accepted","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"},
             "attendees":[{"self":true,"responseStatus":"accepted"},{"responseStatus":"declined"}]},
            {"id":"garbled","start":{"dateTime":"yesterday"},"end":{"dateTime":"2026-11-06T11:00:00Z"}},
            {"id":"backwards","start":{"dateTime":"2026-11-06T12:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}},
            {"start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T11:00:00Z"}}
            ]}"#,
        );
        let ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["accepted"]);
    }

    #[test]
    fn detects_video_links_and_defaults_the_title() {
        let events = parse(
            r#"{"items":[
            {"id":"meet","summary":"  Standup  ","hangoutLink":"https://meet.google.com/x","start":{"dateTime":"2026-11-06T10:00:00Z"},"end":{"dateTime":"2026-11-06T10:15:00Z"}},
            {"id":"zoom","start":{"dateTime":"2026-11-06T11:00:00Z"},"end":{"dateTime":"2026-11-06T11:15:00Z"},
             "conferenceData":{"entryPoints":[{"entryPointType":"phone"},{"entryPointType":"video"}]}},
            {"id":"phone","summary":"Call","start":{"dateTime":"2026-11-06T12:00:00Z"},"end":{"dateTime":"2026-11-06T12:15:00Z"},
             "conferenceData":{"entryPoints":[{"entryPointType":"phone"}]}}
            ]}"#,
        );
        assert!(events[0].has_video && events[0].title == "Standup");
        assert!(events[1].has_video && events[1].title == NO_TITLE);
        assert!(!events[2].has_video);
    }

    #[test]
    fn long_titles_are_cut_and_garbage_is_an_error() {
        let long = "x".repeat(500);
        let json = format!(
            r#"{{"items":[{{"id":"a","summary":"{long}","start":{{"dateTime":"2026-11-06T10:00:00Z"}},"end":{{"dateTime":"2026-11-06T11:00:00Z"}}}}]}}"#
        );
        assert_eq!(parse(&json)[0].title.chars().count(), MAX_TITLE_CHARS);
        assert!(parse_events(b"not json").is_err());
        assert!(parse(r"{}").is_empty());
    }

    #[test]
    fn debug_does_not_print_the_title() {
        let e = CalendarEvent {
            id: "secret-id".into(),
            title: "Layoffs".into(),
            start_ms: 1,
            end_ms: 2,
            has_video: false,
        };
        let text = format!("{e:?}");
        assert!(!text.contains("Layoffs") && !text.contains("secret-id"));
    }

    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339(1_793_959_200_000).unwrap(), "2026-11-06T10:00:00Z");
    }
}
