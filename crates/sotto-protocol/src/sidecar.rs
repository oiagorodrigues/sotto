//! Messages between the Rust core and the `sotto-audio` Swift sidecar.
//!
//! Transport (ADR 0007): commands as JSON lines on the sidecar's stdin, events as JSON lines on
//! its stdout, raw PCM for the cloud engine as binary frames on file descriptor 3. The Swift
//! side mirrors these types in `native/sotto-audio/Sources/SottoProtocol`; the fixtures in
//! `tests/fixtures/sidecar` are decoded by both test suites to keep them in sync.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;

/// Sample rate of PCM frames on fd 3 (mono, signed 16-bit little endian).
pub const PCM_SAMPLE_RATE_HZ: u32 = 16_000;

/// Size of a PCM frame header on fd 3: speaker (u8), start_ms (u64 LE), sample_count (u32 LE).
/// The header is followed by `sample_count` i16 LE samples.
pub const PCM_FRAME_HEADER_BYTES: usize = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Speaker {
    Me,
    Them,
}

impl Speaker {
    #[must_use]
    pub const fn pcm_tag(self) -> u8 {
        match self {
            Self::Me => 0,
            Self::Them => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EngineKind {
    /// On-device `SpeechTranscriber`; the sidecar emits `Transcript` events.
    Local,
    /// Cloud engine; the sidecar emits PCM frames on fd 3 and the core streams them.
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionKind {
    Microphone,
    SystemAudio,
    SpeechRecognition,
    ScreenRecording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionStatus {
    Granted,
    Denied,
    NotDetermined,
    Restricted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PermissionState {
    pub permission: PermissionKind,
    pub status: PermissionStatus,
}

/// Core to sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SidecarCommand {
    Start {
        /// BCP 47 identifier, for example `pt-BR`.
        locale: String,
        engine: EngineKind,
        speakers: Vec<Speaker>,
    },
    Stop {},
    /// Ask the analyzer for this speaker to finalize everything up to `through_ms`.
    Finalize {
        speaker: Speaker,
        through_ms: u64,
    },
    CaptureScreenshot {
        request_id: Uuid,
        max_long_side_px: u32,
    },
    CheckPermissions {},
    RequestPermission {
        permission: PermissionKind,
    },
    Shutdown {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarErrorCode {
    UnsupportedLocale,
    AssetsUnavailable,
    PermissionDenied,
    AudioDevice,
    Analyzer,
    Screenshot,
    BadCommand,
    Internal,
}

/// Sidecar to core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SidecarEvent {
    Ready {
        protocol_version: u32,
        sidecar_version: String,
        /// Locales the local engine supports and has installed.
        installed_locales: Vec<String>,
    },
    /// Capture began. Every later `*_ms` value is relative to this host-time origin.
    /// `emitted_at_ms` is the sidecar clock (ms since t0) when this event was written, so the
    /// core can place t0 on its own clock as `now - emitted_at_ms` even though setup (asset
    /// installation, engine and tap start) happened between t0 and this event.
    Started {
        t0_host_ns: u64,
        emitted_at_ms: u64,
    },
    /// One revision of a transcript segment. A later revision with the same `segment_id`
    /// replaces this one; an empty `text` revokes the segment.
    Transcript {
        speaker: Speaker,
        segment_id: u64,
        revision: u32,
        start_ms: u64,
        end_ms: u64,
        text: String,
        is_final: bool,
    },
    /// Input level, about ten per second per speaker.
    Level {
        speaker: Speaker,
        dbfs: f32,
    },
    Screenshot {
        request_id: Uuid,
        png_base64: String,
        width_px: u32,
        height_px: u32,
    },
    Permissions {
        states: Vec<PermissionState>,
    },
    /// Buffers dropped because a bounded queue was full. Cumulative per speaker.
    Overflow {
        speaker: Speaker,
        dropped_buffers: u64,
    },
    Error {
        code: SidecarErrorCode,
        message: String,
        fatal: bool,
    },
    Stopped {},
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_event_round_trips() -> Result<(), serde_json::Error> {
        let event = SidecarEvent::Transcript {
            speaker: Speaker::Them,
            segment_id: 7,
            revision: 3,
            start_ms: 1_200,
            end_ms: 2_480,
            text: "como você faria".into(),
            is_final: false,
        };
        let json = serde_json::to_string(&event)?;
        assert!(json.contains(r#""type":"transcript""#));
        assert!(json.contains(r#""speaker":"them""#));
        assert_eq!(serde_json::from_str::<SidecarEvent>(&json)?, event);
        Ok(())
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{"type":"stopped","extra":1}"#;
        assert!(serde_json::from_str::<SidecarEvent>(json).is_err());
        let json = r#"{"type":"finalize","speaker":"me","through_ms":10,"x":true}"#;
        assert!(serde_json::from_str::<SidecarCommand>(json).is_err());
    }

    #[test]
    fn shared_fixtures_decode() -> Result<(), Box<dyn std::error::Error>> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sidecar");
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let text = std::fs::read_to_string(&path)?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name.starts_with("event_") {
                serde_json::from_str::<SidecarEvent>(&text).map_err(|e| format!("{name}: {e}"))?;
            } else if name.starts_with("command_") {
                serde_json::from_str::<SidecarCommand>(&text)
                    .map_err(|e| format!("{name}: {e}"))?;
            }
        }
        Ok(())
    }
}
