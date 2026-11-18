//! The account task against a fake backend (wiremock) and the in-memory secret store.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sotto_protocol::api::{
    CLIENT_ID, Preferences, PreferencesConflict, PreferencesDocument, REDIRECT_URI, SyncedMode,
    SyncedSettings,
};
use sotto_protocol::ui::{AccountState, AccountView};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{AccountConfig, AccountHandle, Applies, BackendUrl, Command, LocalSettings, spawn};
use crate::calendar::{UrlOpener, challenge_for};
use crate::secrets::{Account, MemoryStore, SecretStore};

const WAIT: Duration = Duration::from_secs(5);
const SETTLE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Browser(Mutex<Vec<String>>);

impl UrlOpener for Browser {
    fn open(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(())
    }
}

struct Harness {
    server: MockServer,
    handle: AccountHandle,
    applies: Applies,
    secrets: Arc<MemoryStore>,
    browser: Arc<Browser>,
}

fn local() -> LocalSettings {
    LocalSettings {
        auto_answer_model: "claude-haiku-5-5".into(),
        manual_model: "claude-sonnet-5-5".into(),
        spend_cap_micro_usd: 500_000,
        locale: "en-US".into(),
        auto_answer: true,
        invisible: true,
    }
}

fn synced(l: &LocalSettings) -> SyncedSettings {
    SyncedSettings {
        auto_answer_model: l.auto_answer_model.clone(),
        manual_model: l.manual_model.clone(),
        spend_cap_micro_usd: l.spend_cap_micro_usd,
        locale: l.locale.clone(),
        auto_answer: l.auto_answer,
        invisible: l.invisible,
        engine_preference: "cloud".into(),
    }
}

fn server_prefs(l: &LocalSettings) -> Preferences {
    Preferences {
        settings: synced(l),
        shortcuts: BTreeMap::from([("toggle_overlay".to_string(), "Cmd+Shift+Space".to_string())]),
        modes: vec![SyncedMode {
            id: "m1".into(),
            name: "Interview".into(),
            system_prompt: "Be brief.".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }],
    }
}

fn doc(version: u64, prefs: Option<Preferences>, updated_at: Option<&str>) -> Value {
    serde_json::to_value(PreferencesDocument {
        version,
        preferences: prefs,
        updated_at: updated_at.map(str::to_string),
    })
    .unwrap()
}

fn tokens(access: &str, refresh: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": 900,
    }))
}

async fn harness(refresh: Option<&str>, login_ttl: Duration) -> Harness {
    let server = MockServer::start().await;
    let secrets = Arc::new(MemoryStore::new());
    if let Some(r) = refresh {
        secrets.set(Account::AuthRefresh, r).unwrap();
    }
    let browser = Arc::new(Browser::default());
    let config = AccountConfig {
        backend: Some(BackendUrl::parse(&server.uri()).unwrap()),
        opener: Arc::clone(&browser) as Arc<dyn UrlOpener>,
        login_ttl,
        push_debounce: Duration::from_millis(60),
        sync_every: Duration::from_secs(3_600),
    };
    let (handle, applies) = spawn(config, Arc::clone(&secrets) as Arc<dyn SecretStore>);
    Harness {
        server,
        handle,
        applies,
        secrets,
        browser,
    }
}

impl Harness {
    async fn wait(&self, what: &str, done: impl Fn(&AccountView) -> bool) -> AccountView {
        let mut rx = self.handle.state();
        let result = tokio::time::timeout(WAIT, async {
            loop {
                {
                    let view = rx.borrow_and_update();
                    if done(&view) {
                        return view.clone();
                    }
                }
                rx.changed().await.expect("the account task stopped");
            }
        })
        .await;
        result.unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", rx.borrow()))
    }

    fn view(&self) -> AccountView {
        self.handle.state().borrow().clone()
    }

    fn authorize_params(&self) -> HashMap<String, String> {
        let urls = self.browser.0.lock().unwrap();
        let url = url::Url::parse(urls.last().expect("the browser was not opened")).unwrap();
        url.query_pairs().into_owned().collect()
    }

    async fn requests(&self, p: &str) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == p)
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
            .collect()
    }

    async fn mount_session(&self, access: &str, email: &str) {
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .and(header("authorization", format!("Bearer {access}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "u1", "email": email, "created_at": "2026-01-01T00:00:00Z"
            })))
            .mount(&self.server)
            .await;
    }

    async fn mount_prefs_get(&self, access: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path("/v1/preferences"))
            .and(header("authorization", format!("Bearer {access}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    async fn mount_put(&self, base_version: u64, response: ResponseTemplate) {
        Mock::given(method("PUT"))
            .and(path("/v1/preferences"))
            .and(body_partial_json(json!({"base_version": base_version})))
            .respond_with(response)
            .mount(&self.server)
            .await;
    }

    /// Signs in through the browser round trip and waits until the account is signed in.
    async fn sign_in(&self, code: &str) {
        self.handle.send(Command::SignIn);
        self.wait("the browser", |v| {
            v.state == AccountState::WaitingForBrowser
        })
        .await;
        let state = self.authorize_params()["state"].clone();
        self.handle
            .deep_link(&format!("sotto://auth/callback?code={code}&state={state}"));
        self.wait("sign-in", |v| v.state == AccountState::SignedIn)
            .await;
    }
}

#[tokio::test]
async fn sign_in_opens_the_login_page_and_exchanges_the_code_with_the_verifier() {
    let h = harness(None, Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_partial_json(json!({
            "grant_type": "authorization_code",
            "code": "code-1",
            "redirect_uri": REDIRECT_URI,
            "client_id": CLIENT_ID,
        })))
        .respond_with(tokens("a1", "r1"))
        .expect(1)
        .mount(&h.server)
        .await;
    h.mount_session("a1", "me@example.com").await;
    h.mount_prefs_get("a1", doc(0, None, None)).await;
    h.mount_put(
        0,
        ResponseTemplate::new(200).set_body_json(doc(
            1,
            Some(server_prefs(&local())),
            Some("2026-11-06T10:00:00Z"),
        )),
    )
    .await;
    h.handle.send(Command::Local(local()));

    h.sign_in("code-1").await;

    let params = h.authorize_params();
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(params["redirect_uri"], REDIRECT_URI);
    assert_eq!(params["code_challenge_method"], "S256");
    let view = h
        .wait("the email", |v| {
            v.email.is_some() && v.last_synced_ms.is_some()
        })
        .await;
    assert_eq!(view.email.as_deref(), Some("me@example.com"));
    assert_eq!(
        h.secrets.get(Account::AuthRefresh).unwrap().as_deref(),
        Some("r1")
    );
    let exchange = &h.requests("/v1/oauth/token").await[0];
    let verifier = exchange["code_verifier"].as_str().unwrap();
    assert_eq!(challenge_for(verifier), params["code_challenge"]);
}

#[tokio::test]
async fn a_callback_with_another_state_is_ignored_and_does_not_cancel_the_login() {
    let h = harness(None, Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(tokens("a1", "r1"))
        .mount(&h.server)
        .await;
    h.mount_session("a1", "me@example.com").await;
    h.handle.send(Command::SignIn);
    h.wait("the browser", |v| {
        v.state == AccountState::WaitingForBrowser
    })
    .await;

    h.handle
        .deep_link("sotto://auth/callback?code=evil&state=not-the-state");
    h.handle.deep_link("sotto://auth/callback?code=evil&state=");
    h.handle.deep_link("https://example.com/?code=evil");
    tokio::time::sleep(SETTLE).await;

    assert_eq!(h.view().state, AccountState::WaitingForBrowser);
    assert!(h.requests("/v1/oauth/token").await.is_empty());
    assert_eq!(h.secrets.get(Account::AuthRefresh).unwrap(), None);

    let state = h.authorize_params()["state"].clone();
    h.handle
        .deep_link(&format!("sotto://auth/callback?code=good&state={state}"));
    h.wait("sign-in", |v| v.state == AccountState::SignedIn)
        .await;
}

#[tokio::test]
async fn a_state_works_once() {
    let h = harness(None, Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(tokens("a1", "r1"))
        .mount(&h.server)
        .await;
    h.mount_session("a1", "me@example.com").await;
    h.sign_in("code-1").await;
    let state = h.authorize_params()["state"].clone();

    h.handle
        .deep_link(&format!("sotto://auth/callback?code=code-2&state={state}"));
    tokio::time::sleep(SETTLE).await;

    assert_eq!(h.requests("/v1/oauth/token").await.len(), 1);
}

#[tokio::test]
async fn an_unsolicited_callback_does_nothing() {
    let h = harness(None, Duration::from_secs(60)).await;
    h.handle
        .deep_link("sotto://auth/callback?code=attacker&state=anything");
    tokio::time::sleep(SETTLE).await;
    assert_eq!(h.view(), AccountView::signed_out());
    assert!(h.requests("/v1/oauth/token").await.is_empty());
    assert_eq!(h.secrets.get(Account::AuthRefresh).unwrap(), None);
}

#[tokio::test]
async fn an_expired_login_rejects_its_own_callback() {
    let h = harness(None, Duration::from_millis(120)).await;
    h.handle.send(Command::SignIn);
    h.wait("the browser", |v| {
        v.state == AccountState::WaitingForBrowser
    })
    .await;
    let state = h.authorize_params()["state"].clone();
    let view = h
        .wait("the timeout", |v| v.state == AccountState::Error)
        .await;
    assert!(view.error.unwrap().contains("too long"));

    h.handle
        .deep_link(&format!("sotto://auth/callback?code=late&state={state}"));
    tokio::time::sleep(SETTLE).await;
    assert_eq!(h.view().state, AccountState::Error);
    assert!(h.requests("/v1/oauth/token").await.is_empty());
}

#[tokio::test]
async fn a_login_the_user_denied_ends_in_an_error() {
    let h = harness(None, Duration::from_secs(60)).await;
    h.handle.send(Command::SignIn);
    h.wait("the browser", |v| {
        v.state == AccountState::WaitingForBrowser
    })
    .await;
    let state = h.authorize_params()["state"].clone();
    h.handle.deep_link(&format!(
        "sotto://auth/callback?error=access_denied&state={state}"
    ));
    let view = h
        .wait("the error", |v| v.state == AccountState::Error)
        .await;
    assert!(view.error.unwrap().contains("cancelled"));
    assert!(h.requests("/v1/oauth/token").await.is_empty());
}

#[tokio::test]
async fn a_rejected_code_exchange_is_an_error_and_stores_nothing() {
    let h = harness(None, Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(
                json!({"error": "invalid_grant", "message": "The grant is invalid."}),
            ),
        )
        .mount(&h.server)
        .await;
    h.handle.send(Command::SignIn);
    h.wait("the browser", |v| {
        v.state == AccountState::WaitingForBrowser
    })
    .await;
    let state = h.authorize_params()["state"].clone();
    h.handle
        .deep_link(&format!("sotto://auth/callback?code=spent&state={state}"));
    h.wait("the error", |v| v.state == AccountState::Error)
        .await;
    assert_eq!(h.secrets.get(Account::AuthRefresh).unwrap(), None);
}

#[tokio::test]
async fn a_stored_refresh_token_is_rotated_before_the_access_token_is_used() {
    let h = harness(Some("r1"), Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .and(body_partial_json(
            json!({"grant_type": "refresh_token", "refresh_token": "r1", "client_id": CLIENT_ID}),
        ))
        .respond_with(tokens("a2", "r2"))
        .expect(1)
        .mount(&h.server)
        .await;
    h.mount_session("a2", "me@example.com").await;
    h.mount_prefs_get("a2", doc(0, None, None)).await;
    h.mount_put(
        0,
        ResponseTemplate::new(200).set_body_json(doc(
            1,
            Some(server_prefs(&local())),
            Some("2026-11-06T10:00:00Z"),
        )),
    )
    .await;
    h.wait("the stored session", |v| v.state == AccountState::SignedIn)
        .await;

    h.handle.send(Command::Local(local()));
    let view = h
        .wait("the first sync", |v| v.last_synced_ms.is_some())
        .await;

    assert_eq!(view.email.as_deref(), Some("me@example.com"));
    assert_eq!(
        h.secrets.get(Account::AuthRefresh).unwrap().as_deref(),
        Some("r2")
    );
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries() {
    let h = harness(Some("r1"), Duration::from_secs(60)).await;
    for (old, access, new) in [("r1", "a1", "r2"), ("r2", "a3", "r3")] {
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .and(body_partial_json(json!({"refresh_token": old})))
            .respond_with(tokens(access, new))
            .expect(1)
            .mount(&h.server)
            .await;
    }
    h.mount_session("a1", "me@example.com").await;
    Mock::given(method("GET"))
        .and(path("/v1/preferences"))
        .and(header("authorization", "Bearer a1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"error": "unauthorized", "message": "A valid access token is required."}),
        ))
        .expect(1)
        .mount(&h.server)
        .await;
    h.mount_prefs_get(
        "a3",
        doc(
            1,
            Some(server_prefs(&local())),
            Some("2026-11-06T10:00:00Z"),
        ),
    )
    .await;

    h.handle.send(Command::Local(local()));
    let view = h
        .wait("the retried sync", |v| v.last_synced_ms.is_some())
        .await;

    assert_eq!(view.sync_error, None);
    assert_eq!(
        h.secrets.get(Account::AuthRefresh).unwrap().as_deref(),
        Some("r3")
    );
}

#[tokio::test]
async fn invalid_grant_on_refresh_signs_out_locally() {
    let h = harness(Some("stale"), Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(
                json!({"error": "invalid_grant", "message": "The grant is invalid."}),
            ),
        )
        .mount(&h.server)
        .await;

    h.handle.send(Command::Local(local()));
    let view = h
        .wait("the sign-out", |v| v.state == AccountState::Error)
        .await;

    assert!(view.error.unwrap().contains("Sign in again"));
    assert_eq!(view.email, None);
    assert_eq!(h.secrets.get(Account::AuthRefresh).unwrap(), None);
}

#[tokio::test]
async fn a_server_error_on_refresh_keeps_the_session() {
    let h = harness(Some("r1"), Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&h.server)
        .await;
    h.handle.send(Command::Local(local()));
    let view = h.wait("the sync error", |v| v.sync_error.is_some()).await;
    assert_eq!(view.state, AccountState::SignedIn);
    assert_eq!(
        h.secrets.get(Account::AuthRefresh).unwrap().as_deref(),
        Some("r1")
    );
}

#[tokio::test]
async fn sign_out_tells_the_backend_and_deletes_the_token_even_if_it_fails() {
    for status in [204, 500] {
        let h = harness(None, Duration::from_secs(60)).await;
        Mock::given(method("POST"))
            .and(path("/v1/oauth/token"))
            .respond_with(tokens("a1", "r1"))
            .mount(&h.server)
            .await;
        h.mount_session("a1", "me@example.com").await;
        Mock::given(method("POST"))
            .and(path("/v1/logout"))
            .and(header("authorization", "Bearer a1"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&h.server)
            .await;
        h.sign_in("code-1").await;

        h.handle.send(Command::SignOut);
        let view = h
            .wait("the sign-out", |v| v.state == AccountState::SignedOut)
            .await;

        assert_eq!(view, AccountView::signed_out());
        assert_eq!(h.secrets.get(Account::AuthRefresh).unwrap(), None);
        h.server.verify().await;
    }
}

#[tokio::test]
async fn the_server_document_is_applied_when_nothing_was_edited_here() {
    let h = harness(Some("r1"), Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(tokens("a2", "r2"))
        .mount(&h.server)
        .await;
    h.mount_session("a2", "me@example.com").await;
    let remote = LocalSettings {
        manual_model: "gpt-5".into(),
        locale: "pt-BR".into(),
        auto_answer: false,
        ..local()
    };
    h.mount_prefs_get(
        "a2",
        doc(4, Some(server_prefs(&remote)), Some("2026-11-06T10:00:00Z")),
    )
    .await;

    h.handle.send(Command::Local(local()));
    let mut h = h;
    let applied = tokio::time::timeout(WAIT, h.applies.recv())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(applied, remote);
    h.wait("the sync", |v| v.last_synced_ms.is_some()).await;
    let puts = h.requests("/v1/preferences").await;
    assert!(
        puts.iter().all(Value::is_null),
        "nothing should be pushed after a pure pull"
    );
}

#[tokio::test]
async fn local_changes_are_pushed_after_the_debounce_with_the_known_version() {
    let h = harness(Some("r1"), Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(tokens("a2", "r2"))
        .mount(&h.server)
        .await;
    h.mount_session("a2", "me@example.com").await;
    h.mount_prefs_get("a2", doc(0, None, None)).await;
    h.mount_put(
        0,
        ResponseTemplate::new(200).set_body_json(doc(
            1,
            Some(server_prefs(&local())),
            Some("2026-11-06T10:00:00Z"),
        )),
    )
    .await;
    let changed = LocalSettings {
        spend_cap_micro_usd: 2_000_000,
        ..local()
    };
    let mut second = server_prefs(&changed);
    second.settings.engine_preference = "cloud".into();
    h.mount_put(
        1,
        ResponseTemplate::new(200).set_body_json(doc(
            2,
            Some(second),
            Some("2026-11-06T10:05:00Z"),
        )),
    )
    .await;

    h.handle.send(Command::Local(local()));
    h.wait("the first push", |v| v.last_synced_ms.is_some())
        .await;
    h.handle.send(Command::Local(changed.clone()));
    h.handle.send(Command::Local(LocalSettings {
        spend_cap_micro_usd: 2_000_000,
        auto_answer: false,
        ..local()
    }));
    let deadline = tokio::time::Instant::now() + WAIT;
    let puts = loop {
        let puts: Vec<Value> = h
            .requests("/v1/preferences")
            .await
            .into_iter()
            .filter(|b| !b.is_null())
            .collect();
        if puts.len() >= 2 || tokio::time::Instant::now() > deadline {
            break puts;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    assert_eq!(
        puts.len(),
        2,
        "edits made within the debounce share one push"
    );
    assert_eq!(puts[0]["base_version"], 0);
    assert_eq!(puts[1]["base_version"], 1);
    let settings = &puts[1]["preferences"]["settings"];
    assert_eq!(settings["spend_cap_micro_usd"], 2_000_000);
    assert_eq!(settings["auto_answer"], false);
    assert_eq!(
        puts[0]["preferences"]["settings"]["engine_preference"],
        "local"
    );
}

async fn conflict_harness(current_updated_at: &str) -> (Harness, LocalSettings) {
    let h = harness(Some("r1"), Duration::from_secs(60)).await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(tokens("a2", "r2"))
        .mount(&h.server)
        .await;
    h.mount_session("a2", "me@example.com").await;
    h.mount_prefs_get(
        "a2",
        doc(
            1,
            Some(server_prefs(&local())),
            Some("2020-01-01T00:00:00Z"),
        ),
    )
    .await;
    let other_device = LocalSettings {
        manual_model: "gpt-5".into(),
        spend_cap_micro_usd: 9_000_000,
        ..local()
    };
    let conflict = PreferencesConflict {
        error: "conflict".into(),
        message: "changed".into(),
        current: serde_json::from_value(doc(
            2,
            Some(server_prefs(&other_device)),
            Some(current_updated_at),
        ))
        .unwrap(),
    };
    h.mount_put(
        1,
        ResponseTemplate::new(409).set_body_json(serde_json::to_value(conflict).unwrap()),
    )
    .await;
    h.handle.send(Command::Local(local()));
    h.wait("the first sync", |v| v.last_synced_ms.is_some())
        .await;
    (h, other_device)
}

#[tokio::test]
async fn on_a_409_a_field_edited_after_the_server_write_wins_and_is_pushed_again() {
    let (mut h, _) = conflict_harness("2020-06-01T00:00:00Z").await;
    h.mount_put(
        2,
        ResponseTemplate::new(200).set_body_json(doc(
            3,
            Some(server_prefs(&local())),
            Some("2026-11-06T10:00:00Z"),
        )),
    )
    .await;

    h.handle.send(Command::Local(LocalSettings {
        locale: "pt-BR".into(),
        ..local()
    }));
    let applied = tokio::time::timeout(WAIT, h.applies.recv())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(applied.locale, "pt-BR", "the local edit survives");
    assert_eq!(
        applied.manual_model, "gpt-5",
        "untouched fields follow the server"
    );
    assert_eq!(applied.spend_cap_micro_usd, 9_000_000);
    let deadline = tokio::time::Instant::now() + WAIT;
    let retry = loop {
        let puts = h.requests("/v1/preferences").await;
        if let Some(b) = puts.iter().find(|b| b["base_version"] == 2) {
            break b.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no retry with the new version"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(retry["preferences"]["settings"]["locale"], "pt-BR");
    assert_eq!(retry["preferences"]["settings"]["manual_model"], "gpt-5");
    assert_eq!(retry["preferences"]["modes"][0]["name"], "Interview");
    assert_eq!(
        retry["preferences"]["shortcuts"]["toggle_overlay"],
        "Cmd+Shift+Space"
    );
}

#[tokio::test]
async fn on_a_409_a_newer_server_write_beats_an_older_local_edit() {
    let (mut h, other) = conflict_harness("2999-01-01T00:00:00Z").await;

    h.handle.send(Command::Local(LocalSettings {
        locale: "pt-BR".into(),
        ..local()
    }));
    let applied = tokio::time::timeout(WAIT, h.applies.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(SETTLE).await;

    assert_eq!(applied, other);
    let versions: Vec<Value> = h
        .requests("/v1/preferences")
        .await
        .iter()
        .filter(|b| !b.is_null())
        .map(|b| b["base_version"].clone())
        .collect();
    assert_eq!(versions, vec![json!(1)], "no second push");
}

#[test]
fn nothing_secret_is_printed_by_debug() {
    let config =
        AccountConfig::enabled("http://localhost:8080", Arc::new(Browser::default())).unwrap();
    let text = format!(
        "{config:?} {:?}",
        Command::DeepLink("sotto://auth/callback?code=SECRETCODE&state=SECRETSTATE".into()),
    );
    assert!(!text.contains("SECRET"));
}
