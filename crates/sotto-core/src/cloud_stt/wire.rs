//! JSON messages of the OpenAI Realtime transcription API (checked against the docs on
//! 2026-11-06, see `docs/decisions/LOG.md`).

use base64::Engine as _;
use serde_json::{Value, json};

/// Transcription-only sessions connect without a model in the URL.
pub const DEFAULT_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";

/// Rate the API expects for `audio/pcm`.
pub const API_SAMPLE_RATE_HZ: u32 = 24_000;

/// Streams deltas as speech arrives and a final text on commit; takes no server VAD.
pub const DEFAULT_MODEL: &str = "gpt-live-transcribe";

/// The `session.update` that configures a transcription session. `turn_detection` is null
/// because `gpt-live-transcribe` rejects server VAD; the client commits turns itself.
/// `language` is an ISO 639-1 code hint (`pt`) and is left out when unknown.
#[must_use]
pub fn session_update(model: &str, language: Option<&str>) -> String {
    let mut transcription = json!({ "model": model });
    if let Some(code) = language {
        transcription["languages"] = json!([code]);
    }
    json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": API_SAMPLE_RATE_HZ },
                    "transcription": transcription,
                    "turn_detection": null
                }
            }
        }
    })
    .to_string()
}

#[must_use]
pub fn append(pcm24: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(pcm24.len() * 2);
    for s in pcm24 {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    json!({
        "type": "input_audio_buffer.append",
        "audio": base64::engine::general_purpose::STANDARD.encode(bytes)
    })
    .to_string()
}

#[must_use]
pub fn commit() -> String {
    json!({ "type": "input_audio_buffer.commit" }).to_string()
}

/// The language hint for a BCP 47 locale: its primary subtag when that is a plain two-letter
/// ISO 639-1 code. Chinese is left out because the API wants regional `zh-*` codes for it.
#[must_use]
pub fn language_hint(locale: &str) -> Option<String> {
    let primary = locale.split('-').next()?.to_ascii_lowercase();
    (primary.len() == 2 && primary.chars().all(|c| c.is_ascii_lowercase()) && primary != "zh")
        .then_some(primary)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    SessionUpdated,
    Committed {
        item_id: String,
    },
    Delta {
        item_id: String,
        text: String,
    },
    Completed {
        item_id: String,
        text: String,
    },
    Failed {
        item_id: Option<String>,
    },
    /// Only the error `code` or `type` is kept; the message could quote request content.
    Error {
        code: String,
    },
    Other,
}

#[must_use]
pub fn parse_server_event(text: &str) -> ServerEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return ServerEvent::Other;
    };
    let str_field = |name: &str| v.get(name).and_then(Value::as_str).map(str::to_string);
    let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "session.updated" | "transcription_session.updated" => ServerEvent::SessionUpdated,
        "input_audio_buffer.committed" => str_field("item_id")
            .map_or(ServerEvent::Other, |item_id| ServerEvent::Committed {
                item_id,
            }),
        "conversation.item.input_audio_transcription.delta" => {
            match (str_field("item_id"), str_field("delta")) {
                (Some(item_id), Some(text)) => ServerEvent::Delta { item_id, text },
                _ => ServerEvent::Other,
            }
        }
        "conversation.item.input_audio_transcription.completed" => {
            match (str_field("item_id"), str_field("transcript")) {
                (Some(item_id), Some(text)) => ServerEvent::Completed { item_id, text },
                _ => ServerEvent::Other,
            }
        }
        "conversation.item.input_audio_transcription.failed" => ServerEvent::Failed {
            item_id: str_field("item_id"),
        },
        "error" => {
            let err = v.get("error");
            let code = err
                .and_then(|e| e.get("code").and_then(Value::as_str))
                .or_else(|| err.and_then(|e| e.get("type").and_then(Value::as_str)))
                .unwrap_or("unknown");
            ServerEvent::Error { code: code.into() }
        }
        _ => ServerEvent::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_update_has_the_documented_shape() {
        let v: Value =
            serde_json::from_str(&session_update("gpt-live-transcribe", Some("pt"))).unwrap();
        assert_eq!(v["type"], "session.update");
        assert_eq!(v["session"]["type"], "transcription");
        let input = &v["session"]["audio"]["input"];
        assert_eq!(input["format"]["type"], "audio/pcm");
        assert_eq!(input["format"]["rate"], 24_000);
        assert_eq!(input["transcription"]["model"], "gpt-live-transcribe");
        assert_eq!(input["transcription"]["languages"], json!(["pt"]));
        assert!(input["turn_detection"].is_null());
        assert!(input.as_object().unwrap().contains_key("turn_detection"));
        // The singular `language` field must not be sent with `languages`.
        assert!(input["transcription"].get("language").is_none());
    }

    #[test]
    fn session_update_without_a_language_has_no_hint() {
        let v: Value = serde_json::from_str(&session_update("m", None)).unwrap();
        assert!(
            v["session"]["audio"]["input"]["transcription"]
                .get("languages")
                .is_none()
        );
    }

    #[test]
    fn append_is_base64_little_endian_pcm() {
        let v: Value = serde_json::from_str(&append(&[1, -2, 0x0102])).unwrap();
        assert_eq!(v["type"], "input_audio_buffer.append");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(v["audio"].as_str().unwrap())
            .unwrap();
        assert_eq!(bytes, vec![1, 0, 0xFE, 0xFF, 0x02, 0x01]);
        assert_eq!(
            serde_json::from_str::<Value>(&commit()).unwrap()["type"],
            "input_audio_buffer.commit"
        );
    }

    #[test]
    fn language_hints() {
        assert_eq!(language_hint("pt-BR").as_deref(), Some("pt"));
        assert_eq!(language_hint("EN").as_deref(), Some("en"));
        assert_eq!(language_hint("zh-Hans-CN"), None);
        assert_eq!(language_hint("fil-PH"), None);
        assert_eq!(language_hint(""), None);
    }

    #[test]
    fn parses_server_events() {
        let p = parse_server_event;
        assert_eq!(
            p(
                r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"i1","content_index":0,"delta":" hi"}"#
            ),
            ServerEvent::Delta {
                item_id: "i1".into(),
                text: " hi".into()
            }
        );
        assert_eq!(
            p(
                r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"i1","transcript":"Hi."}"#
            ),
            ServerEvent::Completed {
                item_id: "i1".into(),
                text: "Hi.".into()
            }
        );
        assert_eq!(
            p(r#"{"type":"input_audio_buffer.committed","item_id":"i1"}"#),
            ServerEvent::Committed {
                item_id: "i1".into()
            }
        );
        assert_eq!(
            p(
                r#"{"type":"conversation.item.input_audio_transcription.failed","item_id":"i2","error":{"message":"secret words"}}"#
            ),
            ServerEvent::Failed {
                item_id: Some("i2".into())
            }
        );
        assert_eq!(
            p(
                r#"{"type":"error","error":{"type":"invalid_request_error","code":"bad_thing","message":"echoes your audio"}}"#
            ),
            ServerEvent::Error {
                code: "bad_thing".into()
            }
        );
        assert_eq!(
            p(r#"{"type":"session.updated","session":{}}"#),
            ServerEvent::SessionUpdated
        );
        assert_eq!(p(r#"{"type":"session.created"}"#), ServerEvent::Other);
        assert_eq!(p("not json"), ServerEvent::Other);
        assert_eq!(
            p(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"i1"}"#),
            ServerEvent::Other
        );
    }
}
