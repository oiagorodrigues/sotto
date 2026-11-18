use sotto_protocol::api::PreferencesDocument;

/// Messages are shown to the user, so they never carry tokens, codes or response bodies.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("the system random source failed")]
    Random,
    #[error("Sotto sign-in is not available in this build")]
    NotConfigured,
    #[error("the backend address is not allowed: {0}")]
    BadBackendUrl(&'static str),
    #[error("could not open the browser: {0}")]
    Browser(String),
    #[error("Sotto could not be reached ({0})")]
    Network(&'static str),
    #[error("Sotto rejected the request ({0})")]
    Rejected(String),
    #[error("Sotto rejected the sign-in code; sign in again")]
    InvalidGrant,
    #[error("Your session ended. Sign in again.")]
    SessionEnded,
    #[error("You are not signed in to Sotto")]
    NotSignedIn,
    #[error("Sotto did not accept the access token")]
    Unauthorized,
    #[error("Too many requests; try again in a moment")]
    RateLimited,
    #[error("Sotto sent a response the app cannot read")]
    BadResponse,
    #[error("Keychain: {0}")]
    Keychain(String),
    #[error("the preferences changed on the server")]
    Conflict(Box<PreferencesDocument>),
    #[error("the preferences kept changing on another device; retrying later")]
    ConflictLoop,
}

impl AccountError {
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
