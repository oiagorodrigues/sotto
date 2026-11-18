//! Google Calendar inside the app (design.md, Calendar): OAuth for installed apps with PKCE and a
//! loopback redirect, a refresh token in the Keychain, an access token and the event list in
//! memory only. The backend never sees any of it.

mod error;
mod events;
mod loopback;
mod oauth;
mod pkce;
mod prompt;
mod service;

use std::sync::Arc;
use std::time::Duration;

use sotto_protocol::ui::{ApiKey, CalendarConnection};
use tokio::sync::{mpsc, watch};

pub use self::error::CalendarError;
pub use self::events::{CalendarEvent, parse_events};
pub use self::loopback::Loopback;
pub use self::oauth::{Endpoints, SCOPE};
pub use self::pkce::{Pkce, challenge_for, state as random_state};
pub use self::prompt::{GRACE_MS, LEAD_MS, pick as pick_prompt};

/// Opens a URL in the user's default browser. The shell implements it; the webview never can.
pub trait UrlOpener: Send + Sync {
    fn open(&self, url: &str) -> Result<(), String>;
}

pub struct CalendarConfig {
    /// When false no task runs and every command is ignored (the state stays not configured).
    pub enabled: bool,
    pub endpoints: Endpoints,
    pub opener: Arc<dyn UrlOpener>,
    pub refresh_interval: Duration,
    /// How long the browser sign-in may take.
    pub flow_timeout: Duration,
}

impl std::fmt::Debug for CalendarConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalendarConfig")
            .field("enabled", &self.enabled)
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct NoBrowser;

impl UrlOpener for NoBrowser {
    fn open(&self, _url: &str) -> Result<(), String> {
        Err("no browser is available".into())
    }
}

impl CalendarConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            endpoints: Endpoints::google(),
            opener: Arc::new(NoBrowser),
            refresh_interval: REFRESH_EVERY,
            flow_timeout: FLOW_TIMEOUT,
        }
    }

    #[must_use]
    pub fn google(opener: Arc<dyn UrlOpener>) -> Self {
        Self {
            enabled: true,
            opener,
            ..Self::disabled()
        }
    }
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

pub const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
pub const FLOW_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct CalendarState {
    pub connection: CalendarConnection,
    pub error: Option<String>,
    pub masked_client_id: Option<String>,
    pub has_client_secret: bool,
    pub events: Vec<CalendarEvent>,
}

impl CalendarState {
    #[must_use]
    pub fn not_configured() -> Self {
        Self {
            connection: CalendarConnection::NotConfigured,
            error: None,
            masked_client_id: None,
            has_client_secret: false,
            events: Vec::new(),
        }
    }
}

pub enum Command {
    SetClient {
        client_id: String,
        client_secret: Option<ApiKey>,
    },
    RemoveClient,
    Connect,
    Disconnect,
    Refresh,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SetClient { .. } => "SetClient",
            Self::RemoveClient => "RemoveClient",
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect",
            Self::Refresh => "Refresh",
        })
    }
}

#[derive(Debug, Clone)]
pub struct CalendarHandle {
    commands: mpsc::UnboundedSender<Command>,
    state: watch::Receiver<CalendarState>,
}

impl CalendarHandle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// A handle fed by the test instead of the calendar task.
    #[cfg(test)]
    pub(crate) fn for_test() -> (
        Self,
        watch::Sender<CalendarState>,
        mpsc::UnboundedReceiver<Command>,
    ) {
        let (commands, rx) = mpsc::unbounded_channel();
        let (state_tx, state) = watch::channel(CalendarState::not_configured());
        (Self { commands, state }, state_tx, rx)
    }

    #[must_use]
    pub fn state(&self) -> watch::Receiver<CalendarState> {
        self.state.clone()
    }
}

/// Must run inside a tokio runtime when `config.enabled`.
#[must_use]
pub fn spawn(
    config: CalendarConfig,
    secrets: Arc<dyn crate::secrets::SecretStore>,
) -> CalendarHandle {
    let (commands, rx) = mpsc::unbounded_channel();
    let (state_tx, state) = watch::channel(CalendarState::not_configured());
    if config.enabled {
        tokio::spawn(service::Actor::new(config, secrets, state_tx).run(rx));
    }
    CalendarHandle { commands, state }
}
