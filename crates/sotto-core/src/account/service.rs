use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sotto_protocol::api::{
    CLIENT_ID, PreferencesDocument, PutPreferences, TokenRequest, TokenResponse,
};
use sotto_protocol::api::{Preferences, REDIRECT_URI};
use sotto_protocol::ui::{AccountState, AccountView};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::api::Client;
use super::error::AccountError;
use super::pending::Pending;
use super::prefs::{self, Dirty, FIELDS, LocalSettings};
use super::url::parse_callback;
use super::{AccountConfig, Command};
use crate::secrets::{Account, SecretStore};

/// Refresh the access token this long before the backend says it expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// A push that loses the race this many times in a row waits for the next sync.
const MAX_PUSH_ATTEMPTS: usize = 3;
const LOGIN_EXPIRED: &str = "Sign-in took too long. Try again.";

#[derive(Clone)]
struct Access {
    token: String,
    expires_at: Instant,
}

impl Access {
    fn from_response(t: &TokenResponse) -> Self {
        Self {
            token: t.access_token.clone(),
            expires_at: Instant::now()
                + Duration::from_secs(t.expires_in).saturating_sub(EXPIRY_MARGIN),
        }
    }

    fn fresh(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

impl std::fmt::Debug for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Access").finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct SyncState {
    /// The version of the last server document seen; 0 when the account has none.
    version: u64,
    server: Option<Preferences>,
    local: Option<LocalSettings>,
    dirty: Dirty,
    push_at: Option<Instant>,
    /// The first sync of this run has been attempted.
    started: bool,
}

pub(super) struct Actor {
    cfg: AccountConfig,
    secrets: Arc<dyn SecretStore>,
    client: Option<Client>,
    view_tx: watch::Sender<AccountView>,
    view: AccountView,
    apply_tx: mpsc::UnboundedSender<LocalSettings>,
    access: Option<Access>,
    pending: Option<Pending>,
    sync: SyncState,
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn parse_ms(rfc3339: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
        .ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

async fn at(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

impl Actor {
    pub(super) fn new(
        cfg: AccountConfig,
        secrets: Arc<dyn SecretStore>,
        view_tx: watch::Sender<AccountView>,
        apply_tx: mpsc::UnboundedSender<LocalSettings>,
    ) -> Self {
        Self {
            cfg,
            secrets,
            client: None,
            view_tx,
            view: AccountView::signed_out(),
            apply_tx,
            access: None,
            pending: None,
            sync: SyncState::default(),
        }
    }

    pub(super) async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let Some(backend) = self.cfg.backend.clone() else {
            return;
        };
        match Client::new(backend) {
            Ok(client) => self.client = Some(client),
            Err(e) => {
                self.fail(&e);
                return;
            }
        }
        self.load_session();
        let period = self.cfg.sync_every;
        let mut tick = tokio::time::interval_at(Instant::now() + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                cmd = commands.recv() => match cmd {
                    Some(cmd) => self.on_command(cmd).await,
                    None => break,
                },
                () = at(self.pending.as_ref().map(Pending::expires_at)) => self.login_expired(),
                () = at(self.sync.push_at) => self.push_due().await,
                _ = tick.tick() => self.sync_cycle().await,
            }
        }
    }

    fn publish(&self) {
        self.view_tx.send_if_modified(|current| {
            if *current == self.view {
                false
            } else {
                current.clone_from(&self.view);
                true
            }
        });
    }

    fn signed_in(&self) -> bool {
        self.view.state == AccountState::SignedIn
    }

    fn fail(&mut self, error: &AccountError) {
        self.fail_with(error.to_string());
    }

    fn fail_with(&mut self, message: impl Into<String>) {
        self.view = AccountView {
            state: AccountState::Error,
            error: Some(message.into()),
            ..AccountView::signed_out()
        };
        self.publish();
    }

    fn load_session(&mut self) {
        match self.secrets.get(Account::AuthRefresh) {
            Ok(Some(_)) => {
                self.view.state = AccountState::SignedIn;
                self.publish();
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("account: keychain read failed");
                self.fail(&AccountError::Keychain(e.to_string()));
            }
        }
    }

    async fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::SignIn => self.start_login(),
            Command::SignOut => self.sign_out().await,
            Command::SyncNow => self.sync_cycle().await,
            Command::DeepLink(url) => self.on_deep_link(&url).await,
            Command::Local(settings) => self.on_local(settings).await,
        }
    }

    // ---- login ----

    fn start_login(&mut self) {
        if self.signed_in() {
            return;
        }
        let Some(client) = &self.client else { return };
        let started = Pending::start(self.cfg.login_ttl).and_then(|(pending, state, challenge)| {
            let url = client.backend().authorize_url(&state, &challenge)?;
            Ok((pending, url))
        });
        let (pending, url) = match started {
            Ok(v) => v,
            Err(e) => return self.fail(&e),
        };
        if let Err(e) = self.cfg.opener.open(&url) {
            tracing::warn!("account: the browser did not open");
            return self.fail(&AccountError::Browser(e));
        }
        self.pending = Some(pending);
        self.view = AccountView {
            state: AccountState::WaitingForBrowser,
            ..AccountView::signed_out()
        };
        self.publish();
    }

    fn login_expired(&mut self) {
        self.pending = None;
        if self.view.state == AccountState::WaitingForBrowser {
            tracing::info!("account: the login expired");
            self.fail_with(LOGIN_EXPIRED);
        }
    }

    async fn on_deep_link(&mut self, raw: &str) {
        let Some(callback) = parse_callback(raw) else {
            tracing::info!("account: ignored a link that is not a sign-in callback");
            return;
        };
        let Some(pending) = self.pending.as_ref() else {
            tracing::info!("account: ignored a sign-in callback with no login in progress");
            return;
        };
        if !pending.matches(&callback.state) {
            tracing::info!("account: ignored a sign-in callback with an unknown state");
            return;
        }
        let Some(pending) = self.pending.take() else {
            return;
        };
        if pending.expired() {
            self.fail_with(LOGIN_EXPIRED);
            return;
        }
        let code = match callback.outcome {
            Ok(code) => code,
            Err(_) => {
                tracing::info!("account: the backend reported a failed login");
                self.fail_with("Sign-in was cancelled.");
                return;
            }
        };
        let request = TokenRequest::AuthorizationCode {
            code,
            code_verifier: pending.into_verifier(),
            redirect_uri: REDIRECT_URI.into(),
            client_id: CLIENT_ID.into(),
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let result = match client.token(&request).await {
            Ok(tokens) => self.accept_tokens(&tokens),
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => {
                tracing::info!("account: signed in");
                self.view = AccountView {
                    state: AccountState::SignedIn,
                    ..AccountView::signed_out()
                };
                self.publish();
                self.sync_cycle().await;
            }
            Err(e) => {
                tracing::warn!("account: the code exchange failed");
                self.fail(&e);
            }
        }
    }

    // ---- tokens ----

    /// The new refresh token is stored before the access token is used, so a crash in between
    /// cannot leave the Keychain holding a token the backend already rotated away.
    fn accept_tokens(&mut self, tokens: &TokenResponse) -> Result<(), AccountError> {
        self.secrets
            .set(Account::AuthRefresh, &tokens.refresh_token)
            .map_err(|e| AccountError::Keychain(e.to_string()))?;
        self.access = Some(Access::from_response(tokens));
        Ok(())
    }

    async fn access_token(&mut self) -> Result<String, AccountError> {
        if let Some(access) = self.access.as_ref().filter(|a| a.fresh()) {
            return Ok(access.token.clone());
        }
        self.refresh().await
    }

    async fn refresh(&mut self) -> Result<String, AccountError> {
        let refresh = match self.secrets.get(Account::AuthRefresh) {
            Ok(Some(token)) => token,
            Ok(None) => {
                self.sign_out_local(Some(&AccountError::SessionEnded));
                return Err(AccountError::SessionEnded);
            }
            Err(e) => return Err(AccountError::Keychain(e.to_string())),
        };
        let client = self.client.clone().ok_or(AccountError::NotConfigured)?;
        let request = TokenRequest::RefreshToken {
            refresh_token: refresh,
            client_id: CLIENT_ID.into(),
        };
        match client.token(&request).await {
            Ok(tokens) => {
                self.accept_tokens(&tokens)?;
                Ok(tokens.access_token)
            }
            Err(AccountError::InvalidGrant) => {
                tracing::info!("account: the refresh token was rejected; signed out");
                self.sign_out_local(Some(&AccountError::SessionEnded));
                Err(AccountError::SessionEnded)
            }
            Err(e) => Err(e),
        }
    }

    /// Runs `call` with a valid access token. A 401 forces one refresh and one retry.
    async fn authed<T, F, Fut>(&mut self, call: F) -> Result<T, AccountError>
    where
        F: Fn(Client, String) -> Fut,
        Fut: Future<Output = Result<T, AccountError>>,
    {
        let client = self.client.clone().ok_or(AccountError::NotConfigured)?;
        let token = self.access_token().await?;
        match call(client.clone(), token).await {
            Err(AccountError::Unauthorized) => {
                self.access = None;
                let token = self.refresh().await?;
                call(client, token).await
            }
            other => other,
        }
    }

    fn sign_out_local(&mut self, why: Option<&AccountError>) {
        if let Err(e) = self.secrets.delete(Account::AuthRefresh) {
            tracing::warn!(error = %e, "account: could not delete the refresh token");
        }
        self.access = None;
        self.pending = None;
        let local = self.sync.local.take();
        self.sync = SyncState {
            local,
            ..SyncState::default()
        };
        self.view = AccountView::signed_out();
        if let Some(e) = why {
            self.view.state = AccountState::Error;
            self.view.error = Some(e.to_string());
        }
        self.publish();
    }

    async fn sign_out(&mut self) {
        if self.signed_in()
            && self
                .authed(|c, t| async move { c.logout(&t).await })
                .await
                .is_err()
        {
            tracing::info!("account: the backend could not be told about the sign-out");
        }
        self.sign_out_local(None);
    }

    // ---- preferences ----

    async fn on_local(&mut self, settings: LocalSettings) {
        match self.sync.local.as_mut() {
            None => self.sync.local = Some(settings),
            Some(current) => {
                let now = unix_ms();
                for field in FIELDS {
                    if current.differs(&settings, field) {
                        self.sync.dirty.insert(field, now);
                    }
                }
                *current = settings;
                if !self.sync.dirty.is_empty() && self.signed_in() {
                    self.sync.push_at = Some(Instant::now() + self.cfg.push_debounce);
                }
            }
        }
        if !self.sync.started {
            self.sync_cycle().await;
        }
    }

    async fn push_due(&mut self) {
        self.sync.push_at = None;
        if !self.signed_in() || self.sync.local.is_none() {
            return;
        }
        self.run_sync(false).await;
    }

    async fn sync_cycle(&mut self) {
        if !self.signed_in() || self.sync.local.is_none() {
            return;
        }
        self.sync.started = true;
        self.run_sync(true).await;
    }

    async fn run_sync(&mut self, pull: bool) {
        self.view.syncing = true;
        self.publish();
        let result = self.sync_inner(pull).await;
        self.view.syncing = false;
        match result {
            Ok(()) => {
                self.view.last_synced_ms = Some(unix_ms());
                self.view.sync_error = None;
            }
            // Signing out already reset the view.
            Err(AccountError::SessionEnded) => {}
            Err(e) => {
                tracing::warn!("account: preference sync failed");
                self.view.sync_error = Some(e.to_string());
            }
        }
        self.publish();
    }

    async fn sync_inner(&mut self, pull: bool) -> Result<(), AccountError> {
        if self.view.email.is_none() {
            let me = self.authed(|c, t| async move { c.me(&t).await }).await?;
            self.view.email = Some(me.email);
            self.publish();
        }
        if pull {
            let doc = self
                .authed(|c, t| async move { c.get_preferences(&t).await })
                .await?;
            self.reconcile(doc);
        }
        self.push_pending().await
    }

    fn needs_push(&self) -> bool {
        match (&self.sync.server, &self.sync.local) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(server), Some(local)) => {
                !self.sync.dirty.is_empty()
                    && LocalSettings::from_synced(&server.settings) != *local
            }
        }
    }

    async fn push_pending(&mut self) -> Result<(), AccountError> {
        for _ in 0..MAX_PUSH_ATTEMPTS {
            if !self.needs_push() {
                return Ok(());
            }
            let Some(local) = self.sync.local.clone() else {
                return Ok(());
            };
            let body = PutPreferences {
                base_version: self.sync.version,
                preferences: prefs::compose(self.sync.server.as_ref(), &local),
            };
            let result = self
                .authed(|c, t| {
                    let body = body.clone();
                    async move { c.put_preferences(&t, &body).await }
                })
                .await;
            match result {
                Ok(doc) => {
                    self.sync.version = doc.version;
                    self.sync.server = doc.preferences;
                    self.sync.dirty.clear();
                    return Ok(());
                }
                Err(AccountError::Conflict(current)) => {
                    tracing::info!(
                        "account: preference conflict; last writer wins per field by update time"
                    );
                    self.reconcile(*current);
                }
                Err(e) => return Err(e),
            }
        }
        Err(AccountError::ConflictLoop)
    }

    /// Takes a server document into account: server values replace local ones except for
    /// fields edited here after the document was written.
    fn reconcile(&mut self, doc: PreferencesDocument) {
        self.sync.version = doc.version;
        let Some(server) = doc.preferences else {
            self.sync.server = None;
            return;
        };
        if let Some(local) = self.sync.local.clone() {
            let updated = doc.updated_at.as_deref().and_then(parse_ms);
            let merged = prefs::merge(&local, &self.sync.dirty, &server.settings, updated);
            let remote = LocalSettings::from_synced(&server.settings);
            self.sync
                .dirty
                .retain(|field, _| merged.differs(&remote, *field));
            if merged != local {
                self.sync.local = Some(merged.clone());
                let _ = self.apply_tx.send(merged);
            }
        }
        self.sync.server = Some(server);
    }
}
