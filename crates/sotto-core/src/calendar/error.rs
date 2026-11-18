use std::time::Duration;

/// Messages are shown to the user, so they never carry tokens, codes, client secrets or event
/// details.
#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
    #[error("the system random source failed")]
    Random,
    #[error("could not listen for the Google redirect: {0}")]
    Listen(String),
    #[error("could not open the browser: {0}")]
    Browser(String),
    #[error("sign-in timed out after {} minutes", .0.as_secs() / 60)]
    Timeout(Duration),
    #[error("Google sign-in was cancelled ({0})")]
    Denied(String),
    #[error("Google could not be reached ({0})")]
    Network(&'static str),
    #[error("Google rejected the request: {0}")]
    Rejected(String),
    #[error("Google access was revoked; connect again")]
    GrantRevoked,
    #[error("Google did not return a refresh token; connect again")]
    NoRefreshToken,
    #[error("Google sent a response Sotto cannot read")]
    BadResponse,
    #[error("no OAuth client is configured")]
    NotConfigured,
    #[error("Keychain: {0}")]
    Keychain(String),
}

impl CalendarError {
    pub(crate) fn from_http(e: &reqwest::Error) -> Self {
        Self::Network(if e.is_timeout() {
            "timed out"
        } else if e.is_connect() {
            "connection failed"
        } else {
            "request failed"
        })
    }
}
