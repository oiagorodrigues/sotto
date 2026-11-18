//! Sign-in to the Sotto backend and preference sync (ADR 0008). The refresh token lives in the
//! Keychain, the access token in memory. The webview never sees a code or a token: the shell hands
//! the `sotto://auth/callback` link to the core, which checks it against the login it started.

mod api;
#[cfg(test)]
mod e2e;
mod error;
mod pending;
mod prefs;
mod service;
#[cfg(test)]
mod tests;
mod url;

use std::sync::Arc;
use std::time::Duration;

use sotto_protocol::api::RealtimeTokenResponse;
use sotto_protocol::ui::{AccountState, AccountView};
use tokio::sync::{mpsc, oneshot, watch};

pub use self::error::AccountError;
pub(crate) use self::prefs::TOMBSTONE_TTL_MS;
pub use self::prefs::{LocalMode, LocalSettings};
pub use self::url::BackendUrl;
use crate::calendar::UrlOpener;
use crate::secrets::SecretStore;

/// How long the browser login may take (ADR 0008).
pub const LOGIN_TTL: Duration = Duration::from_secs(5 * 60);
/// Local changes are pushed this long after the last one.
pub const PUSH_DEBOUNCE: Duration = Duration::from_secs(2);
/// Preferences are pulled again this often while signed in.
pub const SYNC_EVERY: Duration = Duration::from_secs(10 * 60);

pub struct AccountConfig {
    /// `None` disables the account: no task runs and the state says why.
    pub backend: Option<BackendUrl>,
    pub opener: Arc<dyn UrlOpener>,
    pub login_ttl: Duration,
    pub push_debounce: Duration,
    pub sync_every: Duration,
}

impl std::fmt::Debug for AccountConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountConfig")
            .field("backend", &self.backend)
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

impl AccountConfig {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            backend: None,
            opener: Arc::new(NoBrowser),
            login_ttl: LOGIN_TTL,
            push_debounce: PUSH_DEBOUNCE,
            sync_every: SYNC_EVERY,
        }
    }

    pub fn enabled(backend: &str, opener: Arc<dyn UrlOpener>) -> Result<Self, AccountError> {
        Ok(Self {
            backend: Some(BackendUrl::parse(backend)?),
            opener,
            ..Self::disabled()
        })
    }
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

pub(crate) enum Command {
    SignIn,
    SignOut,
    SyncNow,
    DeepLink(String),
    /// The settings this device has now, reported after every change and once at start.
    Local(LocalSettings),
    /// A short-lived cloud transcription token, minted by the backend.
    RealtimeToken(oneshot::Sender<Result<RealtimeTokenResponse, AccountError>>),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SignIn => "SignIn",
            Self::SignOut => "SignOut",
            Self::SyncNow => "SyncNow",
            Self::DeepLink(_) => "DeepLink",
            Self::Local(_) => "Local",
            Self::RealtimeToken(_) => "RealtimeToken",
        })
    }
}

#[derive(Debug, Clone)]
pub struct AccountHandle {
    commands: mpsc::UnboundedSender<Command>,
    state: watch::Receiver<AccountView>,
}

impl AccountHandle {
    pub(crate) fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Hands a link the shell received to the core. Anything that is not a sign-in callback for
    /// a login this process started is ignored.
    pub fn deep_link(&self, url: &str) {
        self.send(Command::DeepLink(url.to_string()));
    }

    /// Asks the backend for a cloud transcription token through the account task, which
    /// refreshes the access token first when it has expired. The token is returned to the
    /// caller and kept nowhere else.
    pub async fn realtime_token(&self) -> Result<RealtimeTokenResponse, AccountError> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::RealtimeToken(tx));
        rx.await.map_err(|_| AccountError::NotConfigured)?
    }

    #[must_use]
    pub fn state(&self) -> watch::Receiver<AccountView> {
        self.state.clone()
    }
}

/// Settings the core must apply because another device changed them.
pub type Applies = mpsc::UnboundedReceiver<LocalSettings>;

/// Must run inside a tokio runtime when the config has a backend.
#[must_use]
pub fn spawn(config: AccountConfig, secrets: Arc<dyn SecretStore>) -> (AccountHandle, Applies) {
    let (commands, rx) = mpsc::unbounded_channel();
    let (apply_tx, applies) = mpsc::unbounded_channel();
    let mut initial = AccountView::signed_out();
    if config.backend.is_none() {
        initial.state = AccountState::Error;
        initial.error = Some(AccountError::NotConfigured.to_string());
    }
    let (state_tx, state) = watch::channel(initial);
    if config.backend.is_some() {
        tokio::spawn(service::Actor::new(config, secrets, state_tx, apply_tx).run(rx));
    }
    (AccountHandle { commands, state }, applies)
}
