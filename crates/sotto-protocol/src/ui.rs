//! Messages between the Rust core and the React webview.
//!
//! The core owns all state and emits an `AppSnapshot` on every change (Tauri event
//! `sotto://snapshot`). The webview sends `Intent`s through the single `dispatch` command.
//! Intents are validated in the core before they touch state (security checklist item 5).
//! Millisecond fields are exported to TypeScript as `number`; they stay far below 2^53.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::onboarding::OnboardingView;
use crate::sidecar::{PermissionKind, Speaker};

pub const MAX_ASK_CHARS: usize = 4_000;
pub const MIN_KEY_CHARS: usize = 20;
pub const MAX_KEY_CHARS: usize = 300;
/// 50 US dollars, in millionths.
pub const MAX_SPEND_CAP_MICRO_USD: u64 = 50_000_000;
pub const MAX_MODE_NAME_CHARS: usize = 80;
pub const MAX_MODE_PROMPT_CHARS: usize = 8_000;
const MAX_OLLAMA_MODEL_CHARS: usize = 100;
const MAX_LOCALE_CHARS: usize = 35;
const OLLAMA_PREFIX: &str = "ollama:";
pub const MAX_CLIENT_ID_CHARS: usize = 200;
pub const MAX_CLIENT_SECRET_CHARS: usize = 200;
pub const MAX_EVENT_ID_CHARS: usize = 1_024;
const CLIENT_ID_SUFFIX: &str = ".apps.googleusercontent.com";

/// Model ids offered for each cloud provider. Ollama ids are whatever is installed locally, so
/// they are checked by shape (`ollama:<name>`) instead.
pub const ANTHROPIC_MODELS: [&str; 3] =
    ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"];
pub const OPENAI_MODELS: [&str; 3] = ["gpt-4o", "gpt-5", "gpt-5-mini"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Phase {
    Onboarding,
    Idle,
    Listening,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AppSnapshot {
    pub phase: Phase,
    pub session: Option<SessionView>,
    /// Most recent lines first is the UI's job; the core sends them in time order.
    pub transcript: Vec<TranscriptLine>,
    pub answer: Option<AnswerView>,
    pub invisible: bool,
    pub auto_answer: bool,
    pub engine: EngineView,
    /// Input level of each side of the call, for the pill's level bars.
    pub levels: LevelsView,
    pub debug: DebugStats,
    pub settings: SettingsView,
    pub calendar: CalendarView,
    pub account: AccountView,
    /// Latest user-facing error that is not part of an answer. Never contains transcript text.
    pub notice: Option<String>,
    pub onboarding: OnboardingView,
    /// The user's modes, by name.
    pub modes: Vec<ModeView>,
    /// The mode the next session starts with; `None` starts without one.
    pub selected_mode_id: Option<Uuid>,
}

/// A named system prompt plus attached files used as context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModeView {
    pub id: Uuid,
    pub name: String,
    pub system_prompt: String,
    pub files: Vec<ModeFileView>,
    /// Estimated tokens of all files together.
    pub total_tokens: u32,
    /// The files are too large for the prompt, so answers use the passages that match the
    /// question instead of the full text.
    pub retrieval: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ModeFileStatus {
    Ready,
    Indexing,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModeFileView {
    pub id: Uuid,
    pub name: String,
    /// Estimated tokens of the extracted text.
    pub tokens: u32,
    pub status: ModeFileStatus,
    /// Embedding progress while `Indexing`; both zero before the file is split.
    pub chunks_done: u32,
    pub chunks_total: u32,
    /// Why indexing failed. Never contains file text.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ProviderId {
    Anthropic,
    Openai,
    Ollama,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProviderView {
    pub provider: ProviderId,
    /// A key is stored (Anthropic, OpenAI) or the local server answers (Ollama).
    pub configured: bool,
    /// First 6 and last 4 characters of the stored key. The full key never leaves the Keychain.
    pub masked_key: Option<String>,
    /// Model ids that can be chosen for this provider.
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SettingsView {
    pub providers: Vec<ProviderView>,
    pub auto_answer_model: String,
    pub manual_model: String,
    /// Cap on automatic-answer spend per session, in millionths of a US dollar.
    #[ts(type = "number")]
    pub spend_cap_micro_usd: u64,
    pub ollama_running: bool,
    /// BCP 47 locale the next session transcribes in, for example `pt-BR`.
    pub locale: String,
    /// Locales the on-device engine has installed, as reported by the audio helper. Empty
    /// until the helper has started once.
    pub available_locales: Vec<String>,
    /// Which engine the next session transcribes with.
    pub engine_preference: EnginePreference,
    /// Credentials for cloud transcription exist: an OpenAI key in the Keychain, or a signed-in
    /// Sotto account that can get a hosted token.
    pub cloud_transcription_available: bool,
}

/// The transcription engine the user chose. `Cloud` sends the audio of both sides of the call
/// to a third party, so it is only used after an explicit choice (ADR 0003).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EnginePreference {
    #[default]
    Local,
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionView {
    pub id: Uuid,
    pub title: String,
    pub mode_name: Option<String>,
    #[ts(type = "number")]
    pub started_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptLine {
    /// Stable across revisions: `"{speaker}-{segment_id}"`.
    pub id: String,
    pub speaker: Speaker,
    #[ts(type = "number")]
    pub start_ms: u64,
    #[ts(type = "number")]
    pub end_ms: u64,
    pub text: String,
    pub is_final: bool,
    pub is_question: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AnswerOrigin {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AnswerState {
    /// Speculation running, hidden until the turn ends (ADR 0006).
    Preparing,
    Streaming,
    Done,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AnswerView {
    pub id: Uuid,
    pub origin: AnswerOrigin,
    pub question: String,
    /// Empty while `Preparing`.
    pub text: String,
    pub state: AnswerState,
    pub model: String,
    /// Time from end of question speech to first rendered token (ADR 0006).
    pub latency_ms: Option<u32>,
    pub question_line_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EngineView {
    Local,
    Cloud,
    Unavailable,
}

/// Highest value of a quantized input level. 0 is silence.
pub const LEVEL_STEPS: u8 = 5;
const LEVEL_FLOOR_DBFS: f32 = -55.0;
const LEVEL_CEIL_DBFS: f32 = -10.0;

/// Input level per speaker in `0..=LEVEL_STEPS`. Quantized so the snapshot only changes when a
/// bar would, instead of on every level report from the audio helper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LevelsView {
    pub me: u8,
    pub them: u8,
}

impl LevelsView {
    /// Maps dBFS linearly from -55 (silence) to -10 (loud speech) onto `0..=LEVEL_STEPS`.
    pub fn quantize(dbfs: f32) -> u8 {
        if dbfs.is_nan() || dbfs <= LEVEL_FLOOR_DBFS {
            return 0;
        }
        let span = LEVEL_CEIL_DBFS - LEVEL_FLOOR_DBFS;
        let steps = f32::from(LEVEL_STEPS);
        let scaled = ((dbfs - LEVEL_FLOOR_DBFS) / span * steps).ceil();
        // Clamped to 1..=LEVEL_STEPS above silence, so the cast cannot truncate.
        scaled.clamp(1.0, steps) as u8
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DebugStats {
    pub speculations_started: u32,
    pub speculations_cancelled: u32,
    pub speculations_revealed: u32,
    /// Estimated spend this session, in millionths of a US dollar.
    #[ts(type = "number")]
    pub spend_micro_usd: u64,
    #[ts(type = "number")]
    pub dropped_buffers: u64,
    pub last_latency_ms: Option<u32>,
    pub p50_latency_ms: Option<u32>,
    pub p95_latency_ms: Option<u32>,
    /// Why the last automatic answer was not started (cooldown, rate limit, spend cap, a model
    /// with no price). Never contains transcript text.
    pub last_guard_block: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CalendarConnection {
    /// No OAuth client id is stored yet.
    NotConfigured,
    Disconnected,
    /// The browser sign-in is open or the code is being exchanged.
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AccountState {
    SignedOut,
    /// The system browser is open on the login page.
    WaitingForBrowser,
    SignedIn,
    Error,
}

/// Sign-in state of the Sotto account. The email is for the Settings page only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AccountView {
    pub state: AccountState,
    pub email: Option<String>,
    /// Why the state is `Error`, or why the user was signed out. Never contains tokens or codes.
    pub error: Option<String>,
    /// A preference sync request is in flight.
    pub syncing: bool,
    #[ts(type = "number | null")]
    pub last_synced_ms: Option<i64>,
    pub sync_error: Option<String>,
}

impl AccountView {
    #[must_use]
    pub fn signed_out() -> Self {
        Self {
            state: AccountState::SignedOut,
            email: None,
            error: None,
            syncing: false,
            last_synced_ms: None,
            sync_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CalendarEventView {
    pub id: String,
    pub title: String,
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub end_ms: i64,
    pub has_video: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CalendarView {
    pub connection: CalendarConnection,
    /// Why the connection is in the `Error` state. Never contains tokens or event details.
    pub error: Option<String>,
    /// First 6 and last 4 characters of the stored OAuth client id.
    pub masked_client_id: Option<String>,
    pub has_client_secret: bool,
    /// Events in the next 24 hours, ordered by start.
    pub events: Vec<CalendarEventView>,
    /// The event the compact notch offers to start right now, if any.
    pub prompt_event_id: Option<String>,
}

impl CalendarView {
    #[must_use]
    pub fn not_configured() -> Self {
        Self {
            connection: CalendarConnection::NotConfigured,
            error: None,
            masked_client_id: None,
            has_client_secret: false,
            events: Vec::new(),
            prompt_event_id: None,
        }
    }
}

/// A provider API key in transit from the webview. `Debug` never prints it, so a logged intent
/// cannot leak it (security checklist item 12).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct ApiKey(String);

impl ApiKey {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Webview to core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export)]
pub enum Intent {
    StartSession {
        mode_id: Option<Uuid>,
    },
    EndSession {},
    Ask {
        text: String,
        with_screenshot: bool,
    },
    CancelAnswer {},
    SetInvisible {
        invisible: bool,
    },
    SetAutoAnswer {
        enabled: bool,
    },
    OpenSettings {},
    SetApiKey {
        provider: ProviderId,
        key: ApiKey,
    },
    RemoveApiKey {
        provider: ProviderId,
    },
    SetModels {
        auto_answer_model: String,
        manual_model: String,
    },
    SetSpendCap {
        #[ts(type = "number")]
        micro_usd: u64,
    },
    /// BCP 47 locale for transcription; the core also checks it against the installed list.
    SetLocale {
        locale: String,
    },
    /// Google Cloud OAuth client (Desktop type). The secret is optional because Google documents
    /// it as optional for the token exchange.
    CalendarSetClient {
        client_id: String,
        client_secret: Option<ApiKey>,
    },
    CalendarRemoveClient {},
    CalendarConnect {},
    /// Revokes the Google grant and deletes the stored refresh token.
    CalendarDisconnect {},
    CalendarRefresh {},
    CalendarDismissPrompt {
        event_id: String,
    },
    /// Starts a session titled after the calendar event.
    StartCalendarSession {
        event_id: String,
    },
    /// The user read the recording notice and accepts responsibility for consent.
    AcknowledgeConsent {},
    /// Asks the audio helper for the current permission states.
    CheckPermissions {},
    /// Shows the system prompt for one permission.
    RequestPermission {
        permission: PermissionKind,
    },
    /// Opens the System Settings pane for one permission. The shell handles it; the core
    /// ignores it.
    OpenPrivacySettings {
        permission: PermissionKind,
    },
    /// Plays the bundled clips through the on-device engine and measures it.
    RunTranscriptionCheck {},
    /// Sends one tiny completion to the manual model. Only ever sent by a click.
    TestModel {},
    CompleteOnboarding {},
    /// Shows the onboarding window again. Ignored while listening.
    ReopenOnboarding {},
    CreateMode {
        name: String,
        system_prompt: String,
    },
    UpdateMode {
        mode_id: Uuid,
        name: String,
        system_prompt: String,
    },
    DeleteMode {
        mode_id: Uuid,
    },
    /// The mode the next session starts with. `StartSession` with no mode uses it.
    SelectMode {
        mode_id: Option<Uuid>,
    },
    /// The shell opens a native file panel and hands the chosen paths to the core. The webview
    /// never sees or sends a path.
    PickModeFiles {
        mode_id: Uuid,
    },
    RemoveModeFile {
        mode_id: Uuid,
        file_id: Uuid,
    },
    /// Indexes the files of a mode again after a failure.
    RetryIndexing {
        mode_id: Uuid,
    },
    /// Opens the browser on the Sotto login page. The callback reaches the core through the
    /// shell, never through the webview.
    SignIn {},
    /// Revokes the session at the backend and deletes the stored refresh token.
    SignOut {},
    /// Pulls the synced preferences and pushes pending local changes.
    SyncNow {},
    /// Engine for the next session. Choosing `Cloud` is the opt-in of ADR 0003.
    SetEnginePreference {
        engine: EnginePreference,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentError {
    AskTooLong,
    AskEmpty,
    KeyLength,
    KeyCharacters,
    KeyPrefix,
    /// Ollama runs locally and has no key.
    NoKeyForProvider,
    UnknownModel,
    SpendCapTooHigh,
    BadLocale,
    BadClientId,
    BadClientSecret,
    BadEventId,
    ModeNameEmpty,
    ModeNameTooLong,
    ModePromptTooLong,
}

/// Whether `id` is a model the settings may select.
#[must_use]
pub fn is_known_model(id: &str) -> bool {
    if ANTHROPIC_MODELS.contains(&id) || OPENAI_MODELS.contains(&id) {
        return true;
    }
    id.strip_prefix(OLLAMA_PREFIX).is_some_and(|name| {
        !name.is_empty()
            && name.len() <= MAX_OLLAMA_MODEL_CHARS
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-' | '/'))
    })
}

/// Shape check for a BCP 47 tag as the speech engine reports them: a 2 or 3 letter language,
/// then subtags of 2 to 8 letters or digits (`pt-BR`, `zh-Hans-CN`).
#[must_use]
pub fn is_bcp47_tag(tag: &str) -> bool {
    if tag.is_empty() || tag.len() > MAX_LOCALE_CHARS {
        return false;
    }
    let mut parts = tag.split('-');
    let language_ok = parts
        .next()
        .is_some_and(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphabetic()));
    language_ok
        && parts.all(|p| (2..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// 10 or more visible characters, no whitespace or control characters, at most `max`.
fn is_token_like(value: &str, max: usize) -> bool {
    let len = value.chars().count();
    (10..=max).contains(&len) && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn validate_key(provider: ProviderId, key: &str) -> Result<(), IntentError> {
    let prefix = match provider {
        ProviderId::Anthropic => "sk-ant-",
        ProviderId::Openai => "sk-",
        ProviderId::Ollama => return Err(IntentError::NoKeyForProvider),
    };
    if key != key.trim() {
        return Err(IntentError::KeyCharacters);
    }
    let len = key.chars().count();
    if !(MIN_KEY_CHARS..=MAX_KEY_CHARS).contains(&len) {
        return Err(IntentError::KeyLength);
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(IntentError::KeyCharacters);
    }
    if !key.starts_with(prefix) {
        return Err(IntentError::KeyPrefix);
    }
    Ok(())
}

fn validate_mode(name: &str, system_prompt: &str) -> Result<(), IntentError> {
    if name.trim().is_empty() {
        return Err(IntentError::ModeNameEmpty);
    }
    if name.chars().count() > MAX_MODE_NAME_CHARS {
        return Err(IntentError::ModeNameTooLong);
    }
    if system_prompt.chars().count() > MAX_MODE_PROMPT_CHARS {
        return Err(IntentError::ModePromptTooLong);
    }
    Ok(())
}

impl Intent {
    /// Checks the parts serde cannot express. Call before applying any intent.
    pub fn validate(&self) -> Result<(), IntentError> {
        match self {
            Self::Ask { text, .. } => {
                if text.trim().is_empty() {
                    return Err(IntentError::AskEmpty);
                }
                if text.chars().count() > MAX_ASK_CHARS {
                    return Err(IntentError::AskTooLong);
                }
            }
            Self::SetApiKey { provider, key } => validate_key(*provider, key.expose())?,
            Self::RemoveApiKey { provider } => {
                if *provider == ProviderId::Ollama {
                    return Err(IntentError::NoKeyForProvider);
                }
            }
            Self::SetModels {
                auto_answer_model,
                manual_model,
            } => {
                if !is_known_model(auto_answer_model) || !is_known_model(manual_model) {
                    return Err(IntentError::UnknownModel);
                }
            }
            Self::SetSpendCap { micro_usd } => {
                if *micro_usd > MAX_SPEND_CAP_MICRO_USD {
                    return Err(IntentError::SpendCapTooHigh);
                }
            }
            Self::SetLocale { locale } => {
                if !is_bcp47_tag(locale) {
                    return Err(IntentError::BadLocale);
                }
            }
            Self::CalendarSetClient {
                client_id,
                client_secret,
            } => {
                if !is_token_like(client_id, MAX_CLIENT_ID_CHARS)
                    || !client_id.ends_with(CLIENT_ID_SUFFIX)
                {
                    return Err(IntentError::BadClientId);
                }
                if let Some(secret) = client_secret
                    && !is_token_like(secret.expose(), MAX_CLIENT_SECRET_CHARS)
                {
                    return Err(IntentError::BadClientSecret);
                }
            }
            Self::CalendarDismissPrompt { event_id } | Self::StartCalendarSession { event_id } => {
                if event_id.is_empty()
                    || event_id.chars().count() > MAX_EVENT_ID_CHARS
                    || event_id.chars().any(char::is_control)
                {
                    return Err(IntentError::BadEventId);
                }
            }
            Self::CreateMode {
                name,
                system_prompt,
            }
            | Self::UpdateMode {
                name,
                system_prompt,
                ..
            } => validate_mode(name, system_prompt)?,
            Self::StartSession { .. }
            | Self::CalendarRemoveClient {}
            | Self::CalendarConnect {}
            | Self::CalendarDisconnect {}
            | Self::CalendarRefresh {}
            | Self::SignIn {}
            | Self::SignOut {}
            | Self::SyncNow {}
            | Self::EndSession {}
            | Self::CancelAnswer {}
            | Self::SetInvisible { .. }
            | Self::SetAutoAnswer { .. }
            | Self::SetEnginePreference { .. }
            | Self::OpenSettings {}
            | Self::AcknowledgeConsent {}
            | Self::CheckPermissions {}
            | Self::RequestPermission { .. }
            | Self::OpenPrivacySettings { .. }
            | Self::RunTranscriptionCheck {}
            | Self::TestModel {}
            | Self::CompleteOnboarding {}
            | Self::ReopenOnboarding {} => {}
            Self::DeleteMode { .. }
            | Self::SelectMode { .. }
            | Self::PickModeFiles { .. }
            | Self::RemoveModeFile { .. }
            | Self::RetryIndexing { .. } => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_rejects_unknown_fields() {
        let json = r#"{"type":"set_invisible","invisible":true,"admin":true}"#;
        assert!(serde_json::from_str::<Intent>(json).is_err());
    }

    #[test]
    fn ask_validation() {
        let empty = Intent::Ask {
            text: "  ".into(),
            with_screenshot: false,
        };
        assert_eq!(empty.validate(), Err(IntentError::AskEmpty));
        let long = Intent::Ask {
            text: "a".repeat(MAX_ASK_CHARS + 1),
            with_screenshot: false,
        };
        assert_eq!(long.validate(), Err(IntentError::AskTooLong));
        let ok = Intent::Ask {
            text: "What did they ask?".into(),
            with_screenshot: true,
        };
        assert_eq!(ok.validate(), Ok(()));
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    const ANT: &str = "sk-ant-api03-abcdefghijklmnop";

    fn key(provider: ProviderId, k: &str) -> Intent {
        Intent::SetApiKey {
            provider,
            key: ApiKey::new(k),
        }
    }

    #[test]
    fn key_validation() {
        assert_eq!(key(ProviderId::Anthropic, ANT).validate(), Ok(()));
        assert_eq!(
            key(ProviderId::Openai, "sk-proj-abcdefghijklmnop").validate(),
            Ok(())
        );
        assert_eq!(
            key(ProviderId::Anthropic, "sk-ant-short").validate(),
            Err(IntentError::KeyLength)
        );
        assert_eq!(
            key(
                ProviderId::Anthropic,
                &format!("sk-ant-{}", "a".repeat(300))
            )
            .validate(),
            Err(IntentError::KeyLength)
        );
        assert_eq!(
            key(ProviderId::Anthropic, "sk-ant-api03 abcdefghijkl").validate(),
            Err(IntentError::KeyCharacters)
        );
        assert_eq!(
            key(ProviderId::Anthropic, "sk-ant-api03-abcdefghij\n").validate(),
            Err(IntentError::KeyCharacters)
        );
        assert_eq!(
            key(ProviderId::Anthropic, " sk-ant-api03-abcdefghij").validate(),
            Err(IntentError::KeyCharacters)
        );
        assert_eq!(
            key(ProviderId::Anthropic, "sk-proj-abcdefghijklmnop").validate(),
            Err(IntentError::KeyPrefix)
        );
        assert_eq!(
            key(ProviderId::Openai, "pk-abcdefghijklmnopqrst").validate(),
            Err(IntentError::KeyPrefix)
        );
        assert_eq!(
            key(ProviderId::Ollama, ANT).validate(),
            Err(IntentError::NoKeyForProvider)
        );
        assert_eq!(
            Intent::RemoveApiKey {
                provider: ProviderId::Ollama
            }
            .validate(),
            Err(IntentError::NoKeyForProvider)
        );
        assert_eq!(
            Intent::RemoveApiKey {
                provider: ProviderId::Openai
            }
            .validate(),
            Ok(())
        );
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let intent = key(ProviderId::Anthropic, ANT);
        assert!(!format!("{intent:?}").contains("abcdefghij"));
        assert!(!format!("{:#?}", intent.clone()).contains(ANT));
    }

    #[test]
    fn key_round_trips_as_a_plain_string() -> Result<(), serde_json::Error> {
        let json = serde_json::to_string(&key(ProviderId::Anthropic, ANT))?;
        assert_eq!(
            json,
            format!(r#"{{"type":"set_api_key","provider":"anthropic","key":"{ANT}"}}"#)
        );
        let bad = r#"{"type":"set_api_key","provider":"anthropic","key":"x","extra":1}"#;
        assert!(serde_json::from_str::<Intent>(bad).is_err());
        Ok(())
    }

    #[test]
    fn model_validation() {
        let set = |a: &str, m: &str| Intent::SetModels {
            auto_answer_model: a.into(),
            manual_model: m.into(),
        };
        assert_eq!(set("claude-haiku-5-5", "gpt-5").validate(), Ok(()));
        assert_eq!(
            set("ollama:qwen3:8b", "claude-sonnet-5-5").validate(),
            Ok(())
        );
        for bad in [
            "",
            "claude-unknown",
            "ollama:",
            "ollama:a b",
            "gpt-9",
            "../x",
        ] {
            assert_eq!(
                set(bad, "gpt-5").validate(),
                Err(IntentError::UnknownModel),
                "{bad}"
            );
            assert_eq!(
                set("gpt-5", bad).validate(),
                Err(IntentError::UnknownModel),
                "{bad}"
            );
        }
    }

    #[test]
    fn spend_cap_validation() {
        let cap = |micro_usd| Intent::SetSpendCap { micro_usd };
        assert_eq!(cap(0).validate(), Ok(()));
        assert_eq!(cap(MAX_SPEND_CAP_MICRO_USD).validate(), Ok(()));
        assert_eq!(
            cap(MAX_SPEND_CAP_MICRO_USD + 1).validate(),
            Err(IntentError::SpendCapTooHigh)
        );
        assert!(
            serde_json::from_str::<Intent>(r#"{"type":"set_spend_cap","micro_usd":-1}"#).is_err()
        );
    }
}

#[cfg(test)]
mod locale_tests {
    use super::*;

    #[test]
    fn locale_validation() {
        let set = |l: &str| Intent::SetLocale { locale: l.into() };
        for ok in ["pt-BR", "en-US", "en", "zh-Hans-CN", "es-419"] {
            assert_eq!(set(ok).validate(), Ok(()), "{ok}");
        }
        for bad in [
            "", "p", "pt_BR", "pt-", "-BR", "pt--BR", "english", "pt-BR ", "pt-B", "../x",
        ] {
            assert_eq!(set(bad).validate(), Err(IntentError::BadLocale), "{bad}");
        }
        assert_eq!(
            set(&format!("en-{}", "a1b2c3d4-".repeat(5))).validate(),
            Err(IntentError::BadLocale)
        );
    }
}

#[cfg(test)]
mod unit_variant_tests {
    use super::Intent;

    #[test]
    fn empty_variants_reject_unknown_fields() {
        assert!(serde_json::from_str::<Intent>(r#"{"type":"end_session"}"#).is_ok());
        assert!(serde_json::from_str::<Intent>(r#"{"type":"end_session","x":1}"#).is_err());
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::*;

    const ID: &str = "123456789-abcdef.apps.googleusercontent.com";

    fn set(id: &str, secret: Option<&str>) -> Intent {
        Intent::CalendarSetClient {
            client_id: id.into(),
            client_secret: secret.map(ApiKey::new),
        }
    }

    #[test]
    fn client_validation() {
        assert_eq!(set(ID, None).validate(), Ok(()));
        assert_eq!(set(ID, Some("GOCSPX-abcdefghij")).validate(), Ok(()));
        for bad in [
            "",
            "abc",
            "123456789-abcdef",
            " 123456789.apps.googleusercontent.com",
        ] {
            assert_eq!(
                set(bad, None).validate(),
                Err(IntentError::BadClientId),
                "{bad}"
            );
        }
        assert_eq!(
            set(ID, Some("short")).validate(),
            Err(IntentError::BadClientSecret)
        );
        assert_eq!(
            set(ID, Some("has space in it, long")).validate(),
            Err(IntentError::BadClientSecret)
        );
    }

    #[test]
    fn client_secret_is_redacted_in_debug() {
        let text = format!("{:?}", set(ID, Some("GOCSPX-abcdefghij")));
        assert!(!text.contains("GOCSPX"));
    }

    #[test]
    fn event_id_validation() {
        let dismiss = |id: &str| Intent::CalendarDismissPrompt {
            event_id: id.into(),
        };
        assert_eq!(dismiss("abc123").validate(), Ok(()));
        assert_eq!(dismiss("").validate(), Err(IntentError::BadEventId));
        assert_eq!(dismiss("a\nb").validate(), Err(IntentError::BadEventId));
        assert_eq!(
            Intent::StartCalendarSession {
                event_id: "x".repeat(MAX_EVENT_ID_CHARS + 1)
            }
            .validate(),
            Err(IntentError::BadEventId)
        );
    }

    #[test]
    fn empty_calendar_intents_round_trip() {
        for t in [
            "calendar_connect",
            "calendar_disconnect",
            "calendar_refresh",
            "calendar_remove_client",
        ] {
            let json = format!(r#"{{"type":"{t}"}}"#);
            assert!(serde_json::from_str::<Intent>(&json).is_ok(), "{t}");
            let extra = format!(r#"{{"type":"{t}","x":1}}"#);
            assert!(serde_json::from_str::<Intent>(&extra).is_err(), "{t}");
        }
    }
}

#[cfg(test)]
mod onboarding_tests {
    use super::*;

    #[test]
    fn onboarding_intents_validate_and_reject_unknown_fields() {
        for json in [
            r#"{"type":"acknowledge_consent"}"#,
            r#"{"type":"check_permissions"}"#,
            r#"{"type":"request_permission","permission":"system_audio"}"#,
            r#"{"type":"open_privacy_settings","permission":"screen_recording"}"#,
            r#"{"type":"run_transcription_check"}"#,
            r#"{"type":"test_model"}"#,
            r#"{"type":"complete_onboarding"}"#,
            r#"{"type":"reopen_onboarding"}"#,
        ] {
            let parsed = serde_json::from_str::<Intent>(json);
            assert!(parsed.is_ok(), "{json}");
            if let Ok(intent) = parsed {
                assert_eq!(intent.validate(), Ok(()), "{json}");
            }
        }
        for json in [
            r#"{"type":"acknowledge_consent","x":1}"#,
            r#"{"type":"test_model","model":"gpt-5"}"#,
            r#"{"type":"request_permission","permission":"camera"}"#,
            r#"{"type":"request_permission"}"#,
            r#"{"type":"open_privacy_settings","permission":"microphone","url":"x"}"#,
        ] {
            assert!(serde_json::from_str::<Intent>(json).is_err(), "{json}");
        }
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    fn create(name: &str, prompt: &str) -> Intent {
        Intent::CreateMode {
            name: name.into(),
            system_prompt: prompt.into(),
        }
    }

    #[test]
    fn mode_text_limits() {
        assert_eq!(create("Sales call", "Be brief.").validate(), Ok(()));
        assert_eq!(create("Sales call", "").validate(), Ok(()));
        assert_eq!(
            create("  ", "x").validate(),
            Err(IntentError::ModeNameEmpty)
        );
        assert_eq!(
            create(&"n".repeat(MAX_MODE_NAME_CHARS + 1), "").validate(),
            Err(IntentError::ModeNameTooLong)
        );
        assert_eq!(
            create(&"n".repeat(MAX_MODE_NAME_CHARS), "").validate(),
            Ok(())
        );
        assert_eq!(
            create("a", &"p".repeat(MAX_MODE_PROMPT_CHARS + 1)).validate(),
            Err(IntentError::ModePromptTooLong)
        );
        let update = Intent::UpdateMode {
            mode_id: Uuid::nil(),
            name: String::new(),
            system_prompt: String::new(),
        };
        assert_eq!(update.validate(), Err(IntentError::ModeNameEmpty));
    }

    #[test]
    fn limits_count_characters_not_bytes() {
        assert_eq!(
            create(&"ç".repeat(MAX_MODE_NAME_CHARS), "").validate(),
            Ok(())
        );
    }

    #[test]
    fn mode_intents_round_trip_and_reject_unknown_fields() {
        let id = Uuid::nil();
        let json = format!(r#"{{"type":"pick_mode_files","mode_id":"{id}"}}"#);
        assert_eq!(
            serde_json::from_str::<Intent>(&json).ok(),
            Some(Intent::PickModeFiles { mode_id: id })
        );
        let with_path =
            format!(r#"{{"type":"pick_mode_files","mode_id":"{id}","path":"/etc/passwd"}}"#);
        assert!(serde_json::from_str::<Intent>(&with_path).is_err());
        let select = r#"{"type":"select_mode","mode_id":null}"#;
        assert_eq!(
            serde_json::from_str::<Intent>(select).ok(),
            Some(Intent::SelectMode { mode_id: None })
        );
    }
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn account_intents_take_no_fields() {
        for (t, intent) in [
            ("sign_in", Intent::SignIn {}),
            ("sign_out", Intent::SignOut {}),
            ("sync_now", Intent::SyncNow {}),
        ] {
            let json = format!(r#"{{"type":"{t}"}}"#);
            assert_eq!(
                serde_json::from_str::<Intent>(&json).ok(),
                Some(intent.clone())
            );
            assert_eq!(intent.validate(), Ok(()));
            let extra = format!(r#"{{"type":"{t}","code":"x"}}"#);
            assert!(serde_json::from_str::<Intent>(&extra).is_err(), "{t}");
        }
    }
}

#[cfg(test)]
mod engine_preference_tests {
    use super::*;

    #[test]
    fn engine_preference_intent_round_trips_and_rejects_extras() -> Result<(), serde_json::Error> {
        let json = r#"{"type":"set_engine_preference","engine":"cloud"}"#;
        let intent = serde_json::from_str::<Intent>(json)?;
        assert_eq!(
            intent,
            Intent::SetEnginePreference {
                engine: EnginePreference::Cloud
            }
        );
        assert_eq!(intent.validate(), Ok(()));
        assert_eq!(serde_json::to_string(&intent)?, json);
        for bad in [
            r#"{"type":"set_engine_preference","engine":"cloud","x":1}"#,
            r#"{"type":"set_engine_preference","engine":"deepgram"}"#,
            r#"{"type":"set_engine_preference"}"#,
        ] {
            assert!(serde_json::from_str::<Intent>(bad).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn the_default_preference_is_local() {
        assert_eq!(EnginePreference::default(), EnginePreference::Local);
    }
}

#[cfg(test)]
mod level_tests {
    use super::*;

    #[test]
    fn quantize_maps_the_range_and_clamps() {
        assert_eq!(LevelsView::quantize(f32::NEG_INFINITY), 0);
        assert_eq!(LevelsView::quantize(f32::NAN), 0);
        assert_eq!(LevelsView::quantize(-70.0), 0);
        assert_eq!(LevelsView::quantize(-54.0), 1);
        assert_eq!(LevelsView::quantize(-30.0), 3);
        assert_eq!(LevelsView::quantize(-10.0), LEVEL_STEPS);
        assert_eq!(LevelsView::quantize(0.0), LEVEL_STEPS);
    }
}
