//! Snapshot types for the first-run flow: recording consent, permissions, the local
//! transcription check and the model test.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::sidecar::PermissionState;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OnboardingView {
    /// The user acknowledged the recording notice. Persisted; completing onboarding needs it.
    pub consent_acknowledged: bool,
    /// Last states the audio helper reported. Empty until the first check.
    pub permissions: Vec<PermissionState>,
    pub check: TranscriptionCheckView,
    pub model_test: ModelTestView,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CheckStatus {
    #[default]
    NotRun,
    Running,
    /// At least one clip was measured.
    Done,
    /// No clip could be measured.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ClipOutcome {
    Measured,
    /// The on-device model for this locale is not installed.
    LocaleNotInstalled,
    /// The engine started and failed, or produced no text.
    EngineFailed,
    /// No bundled clip exists for the locale.
    NoSample,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ClipResultView {
    pub locale: String,
    pub outcome: ClipOutcome,
    /// From the first spoken sound to the first partial text.
    pub first_partial_ms: Option<u32>,
    /// From the end of speech to the final text.
    pub final_ms: Option<u32>,
    /// Word error rate against the clip's reference text, in percent.
    pub error_rate_pct: Option<f32>,
    /// Why the clip was not measured. Never contains transcript text.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptionCheckView {
    pub status: CheckStatus,
    /// Clips finished and clips planned in the run in progress.
    pub clips_done: u32,
    pub clips_total: u32,
    /// Locale of the clip being played.
    pub running_locale: Option<String>,
    pub results: Vec<ClipResultView>,
    /// macOS version the shown results were measured on.
    pub macos_version: Option<String>,
    #[ts(type = "number | null")]
    pub checked_at_unix_ms: Option<i64>,
    /// The macOS version changed since the results were measured.
    pub stale: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ModelTestStatus {
    #[default]
    Idle,
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelTestView {
    pub status: ModelTestStatus,
    pub model: Option<String>,
    /// Time to the first token of the reply.
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
}
