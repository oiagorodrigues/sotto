//! Wire types of the `sotto-api` backend (JSON over HTTPS, `snake_case`).
//!
//! The webview never talks to the backend; the Rust core does. Requests reject unknown fields.
//! Timestamps are RFC 3339 strings in UTC. No type here carries meeting content (ADR 0002).
//!
//! Endpoints and their types:
//!
//! | Endpoint | Request | Response |
//! |---|---|---|
//! | `GET /authorize` (browser) | [`AuthorizeQuery`] | HTML login page |
//! | `POST /v1/oauth/token` | [`TokenRequest`] | [`TokenResponse`] |
//! | `POST /v1/logout` | none (bearer) | `204` |
//! | `GET /v1/me` | none (bearer) | [`MeResponse`] |
//! | `GET /v1/preferences` | none (bearer) | [`PreferencesDocument`] |
//! | `PUT /v1/preferences` | [`PutPreferences`] | [`PreferencesDocument`], or `409` with [`PreferencesConflict`] |
//! | `POST /v1/realtime-tokens` | [`RealtimeTokenRequest`] | [`RealtimeTokenResponse`] |
//! | `GET /healthz` | none | [`HealthResponse`] |
//!
//! Every error status uses [`ErrorBody`]. Rate limited responses are `429` with a `Retry-After`
//! header (seconds). Protected endpoints answer `401` with a `WWW-Authenticate: Bearer` header
//! when the access token is missing, unknown, expired or revoked.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The only OAuth client the backend knows.
pub const CLIENT_ID: &str = "sotto-desktop";
/// The only redirect URI the backend accepts, compared byte for byte.
pub const REDIRECT_URI: &str = "sotto://auth/callback";
/// Access token lifetime in seconds.
pub const ACCESS_TOKEN_TTL_SECS: u64 = 15 * 60;
/// Refresh token lifetime in seconds. Every rotation issues a new token with a fresh lifetime.
pub const REFRESH_TOKEN_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// Authorization code lifetime in seconds.
pub const AUTH_CODE_TTL_SECS: u64 = 60;
/// Largest `PUT /v1/preferences` body the server reads.
pub const MAX_PREFERENCES_BODY_BYTES: usize = 512 * 1024;
/// Most modes one account can sync.
pub const MAX_MODES: usize = 100;
/// Longest mode system prompt, in characters.
pub const MAX_PROMPT_CHARS: usize = 8_000;

/// Query string of the browser login page. The core builds this URL and opens it in the system
/// browser. `state` is 1 to 128 characters of `[A-Za-z0-9._~-]`; `code_challenge` is the
/// 43 character base64url (no padding) SHA-256 of the PKCE verifier. `code_challenge_method`
/// must be `S256` and `response_type` must be `code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizeQuery {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
}

/// Body of `POST /v1/oauth/token`, selected by `grant_type`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "grant_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TokenRequest {
    /// Exchange the code from the callback. `code_verifier` is 43 to 128 characters of
    /// `[A-Za-z0-9._~-]` whose SHA-256 (base64url) equals the challenge sent to `/authorize`.
    AuthorizationCode {
        code: String,
        code_verifier: String,
        redirect_uri: String,
        client_id: String,
    },
    /// Rotate a refresh token. The old token stops working; presenting it again revokes the
    /// whole token family.
    RefreshToken {
        refresh_token: String,
        client_id: String,
    },
}

impl std::fmt::Debug for TokenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthorizationCode { .. } => f.write_str("TokenRequest::AuthorizationCode(..)"),
            Self::RefreshToken { .. } => f.write_str("TokenRequest::RefreshToken(..)"),
        }
    }
}

/// Successful token response. Both tokens are opaque; store the refresh token in the Keychain.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    /// Always `Bearer`.
    pub token_type: String,
    /// Seconds until the access token expires.
    pub expires_in: u64,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// Error body for every non-2xx JSON response. `error` is a stable machine code
/// (`invalid_request`, `invalid_grant`, `unauthorized`, `rate_limited`, `conflict`,
/// `payload_too_large`, `upstream_unavailable`, `internal`); `message` is for humans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    pub message: String,
}

/// `GET /v1/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeResponse {
    pub id: String,
    pub email: String,
    pub created_at: String,
}

/// Synced settings. Provider keys are never part of this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncedSettings {
    pub auto_answer_model: String,
    pub manual_model: String,
    /// Cap on automatic-answer spend per session, in millionths of a US dollar.
    pub spend_cap_micro_usd: u64,
    /// BCP 47 locale, for example `pt-BR`.
    pub locale: String,
    pub auto_answer: bool,
    pub invisible: bool,
    /// `local`, `cloud` or `auto`.
    pub engine_preference: String,
}

/// A mode: a named system prompt. Attached files stay on the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncedMode {
    /// Client generated id, 1 to 64 characters of `[A-Za-z0-9_-]`.
    pub id: String,
    pub name: String,
    /// Empty for a deleted mode.
    pub system_prompt: String,
    /// RFC 3339 time of the last local edit, or of the deletion.
    pub updated_at: String,
    /// A tombstone: the mode was deleted at `updated_at`. Lets a deletion reach other devices.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

/// The synced document without its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub settings: SyncedSettings,
    /// Action name to accelerator, for example `"toggle_overlay": "Cmd+Shift+Space"`.
    pub shortcuts: BTreeMap<String, String>,
    pub modes: Vec<SyncedMode>,
}

/// `GET /v1/preferences` and the success body of `PUT`. A new account has version 0 and
/// `preferences: null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencesDocument {
    pub version: u64,
    pub preferences: Option<Preferences>,
    /// RFC 3339 time of the last accepted write, `null` at version 0.
    pub updated_at: Option<String>,
}

/// `PUT /v1/preferences`. `base_version` is the version the client last read (0 for none); the
/// write succeeds only if the server still holds that version, and the response carries
/// `base_version + 1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutPreferences {
    pub base_version: u64,
    pub preferences: Preferences,
}

/// The `409` body of a stale `PUT`: the error plus the document the client must merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesConflict {
    pub error: String,
    pub message: String,
    pub current: PreferencesDocument,
}

/// Cloud transcription providers the backend can mint tokens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeProvider {
    Openai,
}

/// `POST /v1/realtime-tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeTokenRequest {
    pub provider: RealtimeProvider,
    /// BCP 47 locale hint for transcription, for example `pt-BR`. Optional.
    #[serde(default)]
    pub locale: Option<String>,
}

/// A short-lived provider token. The client connects to the provider with it directly; the
/// backend sees no audio.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeTokenResponse {
    pub provider: RealtimeProvider,
    pub token: String,
    /// RFC 3339 expiry of `token`.
    pub expires_at: String,
    /// `true` when the server runs the fake provider (development); the token is useless upstream.
    pub fake: bool,
}

impl std::fmt::Debug for RealtimeTokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeTokenResponse")
            .field("provider", &self.provider)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// `GET /healthz`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_request_rejects_unknown_fields_and_grants() {
        let ok =
            r#"{"grant_type":"refresh_token","refresh_token":"x","client_id":"sotto-desktop"}"#;
        assert!(serde_json::from_str::<TokenRequest>(ok).is_ok());
        let extra =
            r#"{"grant_type":"refresh_token","refresh_token":"x","client_id":"c","scope":"a"}"#;
        assert!(serde_json::from_str::<TokenRequest>(extra).is_err());
        let other = r#"{"grant_type":"password","username":"a","password":"b"}"#;
        assert!(serde_json::from_str::<TokenRequest>(other).is_err());
    }

    #[test]
    fn debug_output_hides_tokens() {
        let t = TokenResponse {
            access_token: "secret-a".into(),
            refresh_token: "secret-r".into(),
            token_type: "Bearer".into(),
            expires_in: 1,
        };
        assert!(!format!("{t:?}").contains("secret"));
        let r = TokenRequest::AuthorizationCode {
            code: "secret-c".into(),
            code_verifier: "secret-v".into(),
            redirect_uri: REDIRECT_URI.into(),
            client_id: CLIENT_ID.into(),
        };
        assert!(!format!("{r:?}").contains("secret"));
    }
}
