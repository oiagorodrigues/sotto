use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sotto_protocol::ui::CalendarConnection;
use tokio::sync::{mpsc, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::error::CalendarError;
use super::events::{CalendarEvent, ListError, list_events};
use super::loopback::Loopback;
use super::oauth::{self, Client, TokenResponse};
use super::pkce::{self, Pkce};
use super::{CalendarConfig, CalendarState, Command, UrlOpener};
use crate::secrets::{Account, SecretStore, SecretsError, mask};

/// Refresh the access token this long before Google says it expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

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

struct Fetched {
    access: Access,
    events: Vec<CalendarEvent>,
}

enum Internal {
    Flow(u64, Result<TokenResponse, CalendarError>),
    Fetch(u64, Result<Fetched, CalendarError>),
}

pub(super) struct Actor {
    cfg: CalendarConfig,
    secrets: Arc<dyn SecretStore>,
    http: Option<reqwest::Client>,
    state_tx: watch::Sender<CalendarState>,
    state: CalendarState,
    client: Option<Client>,
    has_refresh: bool,
    access: Option<Access>,
    flow: Option<AbortHandle>,
    fetching: bool,
    /// Bumped whenever the grant changes, so results of older tasks are dropped.
    epoch: u64,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: Option<mpsc::UnboundedReceiver<Internal>>,
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl Actor {
    pub(super) fn new(
        cfg: CalendarConfig,
        secrets: Arc<dyn SecretStore>,
        state_tx: watch::Sender<CalendarState>,
    ) -> Self {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        Self {
            cfg,
            secrets,
            http: None,
            state_tx,
            state: CalendarState::not_configured(),
            client: None,
            has_refresh: false,
            access: None,
            flow: None,
            fetching: false,
            epoch: 0,
            internal_tx,
            internal_rx: Some(internal_rx),
        }
    }

    pub(super) async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let Some(mut internal) = self.internal_rx.take() else {
            return;
        };
        match oauth::http_client() {
            Ok(http) => self.http = Some(http),
            Err(e) => {
                self.set(CalendarConnection::Error, Some(e.to_string()));
                return;
            }
        }
        self.load().await;
        self.fetch_now().await;
        let period = self.cfg.refresh_interval;
        let mut tick = tokio::time::interval_at(Instant::now() + period, period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                cmd = commands.recv() => match cmd {
                    Some(cmd) => self.on_command(cmd).await,
                    None => break,
                },
                Some(msg) = internal.recv() => self.on_internal(msg).await,
                _ = tick.tick() => self.fetch_now().await,
            }
        }
        if let Some(flow) = self.flow.take() {
            flow.abort();
        }
    }

    fn publish(&self) {
        self.state_tx.send_if_modified(|current| {
            if *current == self.state {
                false
            } else {
                current.clone_from(&self.state);
                true
            }
        });
    }

    fn set(&mut self, connection: CalendarConnection, error: Option<String>) {
        self.state.connection = connection;
        self.state.error = error;
        self.publish();
    }

    async fn keychain<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn SecretStore) -> Result<T, SecretsError> + Send + 'static,
    ) -> Result<T, CalendarError> {
        let secrets = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || f(secrets.as_ref()))
            .await
            .map_err(|_| CalendarError::Keychain("task stopped".into()))?
            .map_err(|e| CalendarError::Keychain(e.to_string()))
    }

    async fn load(&mut self) {
        let loaded = self
            .keychain(|s| {
                Ok((
                    s.get(Account::CalendarClientId)?,
                    s.get(Account::CalendarClientSecret)?,
                    s.get(Account::CalendarRefresh)?.is_some(),
                ))
            })
            .await;
        match loaded {
            Ok((Some(id), secret, has_refresh)) => {
                self.state.masked_client_id = Some(mask(&id));
                self.state.has_client_secret = secret.is_some();
                self.client = Some(Client { id, secret });
                self.has_refresh = has_refresh;
                let connection = if has_refresh {
                    CalendarConnection::Connected
                } else {
                    CalendarConnection::Disconnected
                };
                self.set(connection, None);
            }
            Ok((None, ..)) => self.set(CalendarConnection::NotConfigured, None),
            Err(e) => {
                tracing::warn!("calendar: keychain read failed");
                self.set(CalendarConnection::Error, Some(e.to_string()));
            }
        }
    }

    async fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::SetClient {
                client_id,
                client_secret,
            } => {
                self.set_client(client_id, client_secret.map(|s| s.expose().to_string()))
                    .await
            }
            Command::RemoveClient => self.remove_client().await,
            Command::Connect => self.connect(),
            Command::Disconnect => {
                let revoke_error = self.drop_grant().await;
                let connection = if self.client.is_some() {
                    CalendarConnection::Disconnected
                } else {
                    CalendarConnection::NotConfigured
                };
                self.set(connection, revoke_error);
            }
            Command::Refresh => self.fetch_now().await,
        }
    }

    /// Stops any running task, revokes the grant at Google (best effort) and forgets it.
    /// Returns a message when Google could not be told, since the grant then stays alive there.
    async fn drop_grant(&mut self) -> Option<String> {
        self.epoch += 1;
        if let Some(flow) = self.flow.take() {
            flow.abort();
        }
        self.fetching = false;
        self.access = None;
        self.state.events.clear();
        let token = self
            .keychain(|s| s.get(Account::CalendarRefresh))
            .await
            .ok()
            .flatten();
        let mut revoke_error = None;
        if let (Some(token), Some(http)) = (token, &self.http)
            && oauth::revoke(http, &self.cfg.endpoints, &token)
                .await
                .is_err()
        {
            tracing::warn!("calendar: revoke failed");
            revoke_error = Some(
                "Google could not be reached to revoke access. Remove Sotto at myaccount.google.com/permissions"
                    .to_string(),
            );
        }
        if let Err(e) = self.keychain(|s| s.delete(Account::CalendarRefresh)).await {
            revoke_error = Some(e.to_string());
        }
        self.has_refresh = false;
        revoke_error
    }

    async fn set_client(&mut self, id: String, secret: Option<String>) {
        let _ = self.drop_grant().await;
        let stored_id = id.clone();
        let stored_secret = secret.clone();
        let saved = self
            .keychain(move |s| {
                s.set(Account::CalendarClientId, &stored_id)?;
                match &stored_secret {
                    Some(v) => s.set(Account::CalendarClientSecret, v),
                    None => s.delete(Account::CalendarClientSecret),
                }
            })
            .await;
        match saved {
            Ok(()) => {
                self.state.masked_client_id = Some(mask(&id));
                self.state.has_client_secret = secret.is_some();
                self.client = Some(Client { id, secret });
                self.set(CalendarConnection::Disconnected, None);
            }
            Err(e) => self.set(CalendarConnection::Error, Some(e.to_string())),
        }
    }

    async fn remove_client(&mut self) {
        let _ = self.drop_grant().await;
        let removed = self
            .keychain(|s| {
                s.delete(Account::CalendarClientId)?;
                s.delete(Account::CalendarClientSecret)
            })
            .await;
        self.client = None;
        self.state.masked_client_id = None;
        self.state.has_client_secret = false;
        match removed {
            Ok(()) => self.set(CalendarConnection::NotConfigured, None),
            Err(e) => self.set(CalendarConnection::Error, Some(e.to_string())),
        }
    }

    fn connect(&mut self) {
        if self.flow.is_some() || self.state.connection == CalendarConnection::Connected {
            return;
        }
        let (Some(client), Some(http)) = (self.client.clone(), self.http.clone()) else {
            self.set(
                CalendarConnection::Error,
                Some(CalendarError::NotConfigured.to_string()),
            );
            return;
        };
        self.set(CalendarConnection::Connecting, None);
        let endpoints = self.cfg.endpoints.clone();
        let opener = Arc::clone(&self.cfg.opener);
        let timeout = self.cfg.flow_timeout;
        let epoch = self.epoch;
        let tx = self.internal_tx.clone();
        let task = tokio::spawn(async move {
            let result = run_flow(&http, &endpoints, &client, opener, timeout).await;
            let _ = tx.send(Internal::Flow(epoch, result));
        });
        self.flow = Some(task.abort_handle());
    }

    async fn on_internal(&mut self, msg: Internal) {
        match msg {
            Internal::Flow(epoch, result) if epoch == self.epoch => {
                self.flow = None;
                self.on_flow_done(result).await;
            }
            Internal::Fetch(epoch, result) if epoch == self.epoch => {
                self.fetching = false;
                self.on_fetched(result).await;
            }
            Internal::Flow(..) | Internal::Fetch(..) => {}
        }
    }

    async fn on_flow_done(&mut self, result: Result<TokenResponse, CalendarError>) {
        let token = match result {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("calendar: sign-in failed");
                self.set(CalendarConnection::Error, Some(e.to_string()));
                return;
            }
        };
        let Some(refresh) = token.refresh_token.clone() else {
            self.set(
                CalendarConnection::Error,
                Some(CalendarError::NoRefreshToken.to_string()),
            );
            return;
        };
        if let Err(e) = self
            .keychain(move |s| s.set(Account::CalendarRefresh, &refresh))
            .await
        {
            self.set(CalendarConnection::Error, Some(e.to_string()));
            return;
        }
        self.has_refresh = true;
        self.access = Some(Access::from_response(&token));
        self.set(CalendarConnection::Connected, None);
        self.fetch_now().await;
    }

    async fn on_fetched(&mut self, result: Result<Fetched, CalendarError>) {
        if self.flow.is_some() {
            return;
        }
        match result {
            Ok(f) => {
                self.access = Some(f.access);
                self.state.events = f.events;
                self.set(CalendarConnection::Connected, None);
            }
            Err(CalendarError::GrantRevoked) => {
                let _ = self.keychain(|s| s.delete(Account::CalendarRefresh)).await;
                self.has_refresh = false;
                self.access = None;
                self.state.events.clear();
                self.set(
                    CalendarConnection::Disconnected,
                    Some(CalendarError::GrantRevoked.to_string()),
                );
            }
            Err(e) => {
                tracing::warn!("calendar: refresh failed");
                self.set(CalendarConnection::Error, Some(e.to_string()));
            }
        }
    }

    async fn fetch_now(&mut self) {
        if self.fetching || self.flow.is_some() || !self.has_refresh {
            return;
        }
        let (Some(client), Some(http)) = (self.client.clone(), self.http.clone()) else {
            return;
        };
        let refresh = match self.keychain(|s| s.get(Account::CalendarRefresh)).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                self.has_refresh = false;
                self.set(CalendarConnection::Disconnected, None);
                return;
            }
            Err(e) => {
                self.set(CalendarConnection::Error, Some(e.to_string()));
                return;
            }
        };
        self.fetching = true;
        let endpoints = self.cfg.endpoints.clone();
        let cached = self.access.clone();
        let epoch = self.epoch;
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let result = fetch(&http, &endpoints, &client, &refresh, cached).await;
            let _ = tx.send(Internal::Fetch(epoch, result));
        });
    }
}

async fn run_flow(
    http: &reqwest::Client,
    endpoints: &oauth::Endpoints,
    client: &Client,
    opener: Arc<dyn UrlOpener>,
    timeout: Duration,
) -> Result<TokenResponse, CalendarError> {
    let pkce = Pkce::generate()?;
    let state = pkce::state()?;
    let listener = Loopback::bind().await?;
    let redirect = listener.redirect_uri()?;
    let url = oauth::authorization_url(endpoints, &client.id, &redirect, &pkce.challenge, &state)?;
    tokio::task::spawn_blocking(move || opener.open(&url))
        .await
        .map_err(|_| CalendarError::Browser("task stopped".into()))?
        .map_err(CalendarError::Browser)?;
    let callback = listener.wait(&state, timeout).await?;
    oauth::exchange_code(
        http,
        endpoints,
        client,
        &callback.code,
        &pkce.verifier,
        &redirect,
    )
    .await
}

async fn access_from_refresh(
    http: &reqwest::Client,
    endpoints: &oauth::Endpoints,
    client: &Client,
    refresh_token: &str,
) -> Result<Access, CalendarError> {
    let t = oauth::refresh(http, endpoints, client, refresh_token).await?;
    Ok(Access::from_response(&t))
}

async fn fetch(
    http: &reqwest::Client,
    endpoints: &oauth::Endpoints,
    client: &Client,
    refresh_token: &str,
    cached: Option<Access>,
) -> Result<Fetched, CalendarError> {
    let mut access = match cached {
        Some(a) if a.fresh() => a,
        _ => access_from_refresh(http, endpoints, client, refresh_token).await?,
    };
    match list_events(http, &endpoints.api_base, &access.token, unix_ms()).await {
        Ok(events) => return Ok(Fetched { access, events }),
        Err(ListError::Other(e)) => return Err(e),
        Err(ListError::Unauthorized) => {}
    }
    access = access_from_refresh(http, endpoints, client, refresh_token).await?;
    match list_events(http, &endpoints.api_base, &access.token, unix_ms()).await {
        Ok(events) => Ok(Fetched { access, events }),
        Err(ListError::Unauthorized) => Err(CalendarError::Rejected(
            "Google refused the access token".into(),
        )),
        Err(ListError::Other(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use sotto_protocol::ui::ApiKey;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::calendar::{Command, Endpoints, challenge_for, spawn};
    use crate::secrets::MemoryStore;

    const CLIENT_ID: &str = "1234567890-abc.apps.googleusercontent.com";

    /// Plays the browser: follows the authorization URL's redirect with the given state rule.
    struct FakeBrowser {
        urls: Mutex<Vec<String>>,
        behaviour: Behaviour,
    }

    #[derive(Clone, Copy)]
    enum Behaviour {
        Approve,
        Ignore,
    }

    impl UrlOpener for FakeBrowser {
        fn open(&self, url: &str) -> Result<(), String> {
            self.urls.lock().unwrap().push(url.to_string());
            if matches!(self.behaviour, Behaviour::Ignore) {
                return Ok(());
            }
            let parsed = url::Url::parse(url).unwrap();
            let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
            let target = format!("{}?code=auth-code&state={}", q["redirect_uri"], q["state"]);
            tokio::spawn(async move {
                let _ = reqwest::get(target).await;
            });
            Ok(())
        }
    }

    struct Rig {
        server: MockServer,
        secrets: Arc<MemoryStore>,
        browser: Arc<FakeBrowser>,
        handle: crate::calendar::CalendarHandle,
    }

    fn events_body() -> serde_json::Value {
        let start = unix_ms() + 3_600_000;
        let iso = |ms: i64| {
            time::OffsetDateTime::from_unix_timestamp(ms / 1000)
                .unwrap()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
        };
        serde_json::json!({"items": [
            {"id": "ev1", "summary": "Design review", "hangoutLink": "https://meet.google.com/x",
             "start": {"dateTime": iso(start)}, "end": {"dateTime": iso(start + 1_800_000)}},
            {"id": "ev2", "status": "cancelled"}
        ]})
    }

    async fn mount_google(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-1", "expires_in": 3599, "refresh_token": "refresh-1"
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-2", "expires_in": 3599
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendar/v3/calendars/primary/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(events_body()))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    async fn rig(behaviour: Behaviour, tweak: impl FnOnce(&mut CalendarConfig)) -> Rig {
        let server = MockServer::start().await;
        mount_google(&server).await;
        let secrets = Arc::new(MemoryStore::new());
        let browser = Arc::new(FakeBrowser {
            urls: Mutex::new(Vec::new()),
            behaviour,
        });
        let mut cfg = CalendarConfig::google(Arc::clone(&browser) as Arc<dyn UrlOpener>);
        cfg.endpoints = Endpoints {
            auth_url: format!("{}/auth", server.uri()),
            token_url: format!("{}/token", server.uri()),
            revoke_url: format!("{}/revoke", server.uri()),
            api_base: format!("{}/calendar/v3", server.uri()),
        };
        tweak(&mut cfg);
        let handle = spawn(cfg, Arc::clone(&secrets) as Arc<dyn SecretStore>);
        Rig {
            server,
            secrets,
            browser,
            handle,
        }
    }

    async fn wait_for(
        handle: &crate::calendar::CalendarHandle,
        pred: impl Fn(&CalendarState) -> bool,
    ) -> CalendarState {
        let mut rx = handle.state();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                {
                    let s = rx.borrow_and_update();
                    if pred(&s) {
                        return s.clone();
                    }
                }
                rx.changed().await.unwrap();
            }
        })
        .await
        .expect("state not reached")
    }

    fn set_client(rig: &Rig, secret: Option<&str>) {
        rig.handle.send(Command::SetClient {
            client_id: CLIENT_ID.into(),
            client_secret: secret.map(ApiKey::new),
        });
    }

    async fn requests_to(server: &MockServer, p: &str) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == p)
            .map(|r| String::from_utf8_lossy(&r.body).to_string())
            .collect()
    }

    #[tokio::test]
    async fn starts_not_configured_and_stores_the_client_in_the_keychain() {
        let rig = rig(Behaviour::Approve, |_| {}).await;
        wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::NotConfigured
        })
        .await;
        set_client(&rig, Some("GOCSPX-secretsecret"));
        let s = wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::Disconnected
        })
        .await;
        assert_eq!(s.masked_client_id.as_deref(), Some("123456….com"));
        assert!(s.has_client_secret);
        assert!(!format!("{s:?}").contains("GOCSPX"));
        assert_eq!(
            rig.secrets
                .get(Account::CalendarClientId)
                .unwrap()
                .as_deref(),
            Some(CLIENT_ID)
        );
        assert_eq!(
            rig.secrets
                .get(Account::CalendarClientSecret)
                .unwrap()
                .as_deref(),
            Some("GOCSPX-secretsecret")
        );
        rig.handle.send(Command::RemoveClient);
        wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::NotConfigured && !s.has_client_secret
        })
        .await;
        assert_eq!(rig.secrets.get(Account::CalendarClientId).unwrap(), None);
    }

    #[tokio::test]
    async fn full_sign_in_stores_only_the_refresh_token_and_lists_events() {
        let rig = rig(Behaviour::Approve, |_| {}).await;
        set_client(&rig, Some("GOCSPX-secretsecret"));
        wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::Disconnected
        })
        .await;
        rig.handle.send(Command::Connect);
        let s = wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::Connected && !s.events.is_empty()
        })
        .await;
        assert_eq!(s.events.len(), 1);
        assert_eq!(s.events[0].title, "Design review");
        assert!(s.events[0].has_video);
        assert_eq!(
            rig.secrets
                .get(Account::CalendarRefresh)
                .unwrap()
                .as_deref(),
            Some("refresh-1")
        );

        let urls = rig.browser.urls.lock().unwrap().clone();
        assert_eq!(urls.len(), 1);
        let q: std::collections::HashMap<_, _> = url::Url::parse(&urls[0])
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(q["scope"], crate::calendar::SCOPE);
        assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
        let exchange = &requests_to(&rig.server, "/token").await[0];
        let verifier = exchange
            .split('&')
            .find_map(|p| p.strip_prefix("code_verifier="))
            .unwrap();
        assert_eq!(challenge_for(verifier), q["code_challenge"]);
        assert!(exchange.contains("client_secret=GOCSPX-secretsecret"));

        let calls = rig.server.received_requests().await.unwrap();
        let events_call = calls
            .iter()
            .find(|r| r.url.path().ends_with("/events"))
            .unwrap();
        let query: std::collections::HashMap<_, _> =
            events_call.url.query_pairs().into_owned().collect();
        assert_eq!(query["singleEvents"], "true");
        assert_eq!(query["orderBy"], "startTime");
        assert_eq!(
            events_call
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer access-1"
        );
    }

    #[tokio::test]
    async fn disconnect_revokes_and_deletes_the_refresh_token() {
        let rig = rig(Behaviour::Approve, |_| {}).await;
        set_client(&rig, None);
        rig.handle.send(Command::Connect);
        wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::Connected && !s.events.is_empty()
        })
        .await;
        rig.handle.send(Command::Disconnect);
        let s = wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::Disconnected
        })
        .await;
        assert!(s.events.is_empty() && s.error.is_none());
        assert_eq!(rig.secrets.get(Account::CalendarRefresh).unwrap(), None);
        assert_eq!(
            CLIENT_ID,
            rig.secrets.get(Account::CalendarClientId).unwrap().unwrap()
        );
        let revokes = requests_to(&rig.server, "/revoke").await;
        assert_eq!(revokes, ["token=refresh-1"]);
    }

    #[tokio::test]
    async fn a_stored_grant_reconnects_at_start_through_a_refresh() {
        let server = MockServer::start().await;
        mount_google(&server).await;
        let secrets = Arc::new(MemoryStore::new());
        secrets.set(Account::CalendarClientId, CLIENT_ID).unwrap();
        secrets
            .set(Account::CalendarRefresh, "refresh-stored")
            .unwrap();
        let mut cfg = CalendarConfig::google(Arc::new(FakeBrowser {
            urls: Mutex::new(Vec::new()),
            behaviour: Behaviour::Ignore,
        }));
        cfg.endpoints = Endpoints {
            auth_url: format!("{}/auth", server.uri()),
            token_url: format!("{}/token", server.uri()),
            revoke_url: format!("{}/revoke", server.uri()),
            api_base: format!("{}/calendar/v3", server.uri()),
        };
        let handle = spawn(cfg, Arc::clone(&secrets) as Arc<dyn SecretStore>);
        let s = wait_for(&handle, |s| !s.events.is_empty()).await;
        assert_eq!(s.connection, CalendarConnection::Connected);
        let refreshes = requests_to(&server, "/token").await;
        assert!(refreshes[0].contains("refresh_token=refresh-stored"));
    }

    #[tokio::test]
    async fn a_revoked_grant_returns_to_disconnected_and_deletes_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;
        let secrets = Arc::new(MemoryStore::new());
        secrets.set(Account::CalendarClientId, CLIENT_ID).unwrap();
        secrets.set(Account::CalendarRefresh, "dead").unwrap();
        let mut cfg = CalendarConfig::google(Arc::new(FakeBrowser {
            urls: Mutex::new(Vec::new()),
            behaviour: Behaviour::Ignore,
        }));
        cfg.endpoints.token_url = format!("{}/token", server.uri());
        let handle = spawn(cfg, Arc::clone(&secrets) as Arc<dyn SecretStore>);
        let s = wait_for(&handle, |s| s.error.is_some()).await;
        assert_eq!(s.connection, CalendarConnection::Disconnected);
        assert_eq!(secrets.get(Account::CalendarRefresh).unwrap(), None);
    }

    #[tokio::test]
    async fn sign_in_that_never_completes_times_out_into_an_error() {
        let rig = rig(Behaviour::Ignore, |c| {
            c.flow_timeout = Duration::from_millis(150)
        })
        .await;
        set_client(&rig, None);
        wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::Disconnected
        })
        .await;
        rig.handle.send(Command::Connect);
        wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::Connecting
        })
        .await;
        let s = wait_for(&rig.handle, |s| s.connection == CalendarConnection::Error).await;
        assert!(s.error.unwrap().contains("timed out"));
        assert_eq!(rig.secrets.get(Account::CalendarRefresh).unwrap(), None);
    }

    #[tokio::test]
    async fn connect_without_a_client_is_an_error() {
        let rig = rig(Behaviour::Approve, |_| {}).await;
        wait_for(&rig.handle, |s| {
            s.connection == CalendarConnection::NotConfigured
        })
        .await;
        rig.handle.send(Command::Connect);
        let s = wait_for(&rig.handle, |s| s.connection == CalendarConnection::Error).await;
        assert!(s.error.is_some());
    }

    #[tokio::test]
    async fn events_refresh_on_the_interval() {
        let rig = rig(Behaviour::Approve, |c| {
            c.refresh_interval = Duration::from_millis(100);
        })
        .await;
        set_client(&rig, None);
        rig.handle.send(Command::Connect);
        wait_for(&rig.handle, |s| !s.events.is_empty()).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let n = rig
                    .server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .filter(|r| r.url.path().ends_with("/events"))
                    .count();
                if n >= 3 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("no periodic refresh");
        rig.handle.send(Command::Refresh);
    }

    #[tokio::test]
    async fn an_expired_access_token_is_refreshed_once_and_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"access_token": "fresh", "expires_in": 3599}),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::header("authorization", "Bearer stale"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::header("authorization", "Bearer fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(events_body()))
            .mount(&server)
            .await;
        let http = oauth::http_client().unwrap();
        let endpoints = Endpoints {
            auth_url: String::new(),
            token_url: format!("{}/token", server.uri()),
            revoke_url: String::new(),
            api_base: format!("{}/calendar/v3", server.uri()),
        };
        let client = Client {
            id: CLIENT_ID.into(),
            secret: None,
        };
        let stale = Access {
            token: "stale".into(),
            expires_at: Instant::now() + Duration::from_secs(600),
        };
        let out = fetch(&http, &endpoints, &client, "rt", Some(stale))
            .await
            .unwrap();
        assert_eq!(out.events.len(), 1);
        assert_eq!(out.access.token, "fresh");
    }

    #[test]
    fn debug_output_has_no_tokens() {
        let a = Access {
            token: "tok-secret".into(),
            expires_at: Instant::now(),
        };
        assert!(!format!("{a:?}").contains("tok-secret"));
        let c = Command::SetClient {
            client_id: CLIENT_ID.into(),
            client_secret: Some(ApiKey::new("GOCSPX-abcdefghij")),
        };
        assert_eq!(format!("{c:?}"), "SetClient");
    }
}
