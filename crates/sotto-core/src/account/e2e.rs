//! A real sign-in against a running `services/api` and Mailpit. Ignored by default:
//!
//! ```text
//! just api-dev        # API on http://localhost:8080, mail in Mailpit
//! SOTTO_E2E_API=http://localhost:8080 cargo test -p sotto-core account::e2e -- --ignored
//! ```
//!
//! A fake browser plays the user: it opens the login page, signs up a fresh address, reads the
//! mailed link from Mailpit and hands the resulting `sotto://` link to the core.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sotto_protocol::ui::{AccountState, AccountView};
use tokio::sync::watch;

use super::{AccountConfig, Applies, Command, LocalSettings, spawn};
use crate::calendar::UrlOpener;
use crate::secrets::{Account, MemoryStore, SecretStore};

const MAILPIT: &str = "http://localhost:8025";
const PASSWORD: &str = "correct horse battery staple";

fn local(cap: u64) -> LocalSettings {
    LocalSettings {
        auto_answer_model: "claude-haiku-5-5".into(),
        manual_model: "claude-sonnet-5-5".into(),
        spend_cap_micro_usd: cap,
        locale: "en-US".into(),
        auto_answer: true,
        invisible: true,
        modes: Vec::new(),
    }
}

struct Browser {
    email: String,
    deliver: Arc<dyn Fn(String) + Send + Sync>,
}

impl UrlOpener for Browser {
    fn open(&self, url: &str) -> Result<(), String> {
        let (url, email) = (url.to_string(), self.email.clone());
        let deliver = Arc::clone(&self.deliver);
        tokio::spawn(async move {
            match browse(&url, &email).await {
                Ok(link) => deliver(link),
                Err(e) => panic!("the fake browser failed: {e}"),
            }
        });
        Ok(())
    }
}

fn attr(html: &str, name: &str) -> Option<String> {
    let at = html.find(&format!("name=\"{name}\" value=\""))?;
    let rest = &html[at..];
    let start = rest.find("value=\"")? + "value=\"".len();
    let end = rest[start..].find('"')?;
    Some(rest[start..start + end].replace("&amp;", "&"))
}

async fn browse(login_url: &str, email: &str) -> Result<String, String> {
    let http = reqwest::Client::new();
    let origin = login_url.split("/authorize").next().ok_or("no origin")?;
    let page = http
        .get(login_url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let cookie = page
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with("sotto_csrf="))
        .and_then(|c| c.split(';').next())
        .ok_or("no csrf cookie")?
        .to_string();
    let html = page.text().await.map_err(|e| e.to_string())?;
    let mut form: Vec<(String, String)> = Vec::new();
    for name in [
        "csrf",
        "response_type",
        "client_id",
        "redirect_uri",
        "state",
        "code_challenge",
        "code_challenge_method",
    ] {
        form.push((name.into(), attr(&html, name).ok_or(format!("no {name}"))?));
    }
    form.push(("email".into(), email.into()));
    form.push(("password".into(), PASSWORD.into()));
    let signup = http
        .post(format!("{origin}/authorize/signup"))
        .header("cookie", cookie)
        .form(&form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !signup.status().is_success() {
        return Err(format!("sign-up answered {}", signup.status()));
    }
    let magic = mailed_link(&http, email).await?;
    let landed = http.get(&magic).send().await.map_err(|e| e.to_string())?;
    let html = landed.text().await.map_err(|e| e.to_string())?;
    let at = html
        .find("href=\"sotto://")
        .ok_or("no sotto link on the page")?
        + "href=\"".len();
    let end = html[at..].find('"').ok_or("unterminated link")?;
    Ok(html[at..at + end].replace("&amp;", "&"))
}

async fn mailed_link(http: &reqwest::Client, email: &str) -> Result<String, String> {
    for _ in 0..50 {
        let found: Value = http
            .get(format!("{MAILPIT}/api/v1/search"))
            .query(&[("query", format!("to:{email}"))])
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if let Some(id) = found["messages"][0]["ID"].as_str() {
            let mail: Value = http
                .get(format!("{MAILPIT}/api/v1/message/{id}"))
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            let text = mail["Text"].as_str().unwrap_or_default();
            let at = text.find("http").ok_or("no link in the mail")?;
            let link: String = text[at..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            return Ok(link);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err("the mail never arrived".into())
}

async fn wait(
    rx: &mut watch::Receiver<AccountView>,
    done: impl Fn(&AccountView) -> bool,
) -> AccountView {
    tokio::time::timeout(Duration::from_secs(20), async {
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
    .await
    .expect("timed out")
}

fn start(api: &str, secrets: &Arc<MemoryStore>, email: &str) -> (super::AccountHandle, Applies) {
    let slot: Arc<std::sync::OnceLock<super::AccountHandle>> = Arc::new(std::sync::OnceLock::new());
    let target = Arc::clone(&slot);
    let browser = Browser {
        email: email.into(),
        deliver: Arc::new(move |link| {
            if let Some(handle) = target.get() {
                handle.deep_link(&link);
            }
        }),
    };
    let config = AccountConfig::enabled(api, Arc::new(browser)).unwrap();
    let (handle, applies) = spawn(config, Arc::clone(secrets) as Arc<dyn SecretStore>);
    let _ = slot.set(handle.clone());
    (handle, applies)
}

#[tokio::test]
#[ignore = "needs `just api-dev` and Mailpit; set SOTTO_E2E_API"]
async fn sign_up_in_the_browser_then_sync_preferences_across_restarts() {
    let api = std::env::var("SOTTO_E2E_API").expect("SOTTO_E2E_API is not set");
    let email = format!("e2e-{}@example.com", uuid::Uuid::now_v7().simple());
    let secrets = Arc::new(MemoryStore::new());

    let (handle, _applies) = start(&api, &secrets, &email);
    let mut rx = handle.state();
    handle.send(Command::Local(local(500_000)));
    handle.send(Command::SignIn);
    let view = wait(&mut rx, |v| {
        v.state == AccountState::SignedIn && v.last_synced_ms.is_some()
    })
    .await;
    assert_eq!(view.email.as_deref(), Some(email.as_str()));
    let first_refresh = secrets.get(Account::AuthRefresh).unwrap().unwrap();

    handle.send(Command::Local(local(750_000)));
    wait(&mut rx, |v| v.syncing).await;
    wait(&mut rx, |v| !v.syncing && v.sync_error.is_none()).await;

    // A restart: a new task with the same Keychain rotates the refresh token and pulls what the
    // first run pushed.
    let (again, mut applies) = start(&api, &secrets, &email);
    let mut rx2 = again.state();
    again.send(Command::Local(local(1)));
    let applied = tokio::time::timeout(Duration::from_secs(20), applies.recv())
        .await
        .expect("no settings were applied")
        .unwrap();
    assert_eq!(applied.spend_cap_micro_usd, 750_000);
    wait(&mut rx2, |v| v.email.is_some()).await;
    assert_ne!(
        secrets.get(Account::AuthRefresh).unwrap().unwrap(),
        first_refresh,
        "the refresh token rotates"
    );

    again.send(Command::SignOut);
    wait(&mut rx2, |v| v.state == AccountState::SignedOut).await;
    assert_eq!(secrets.get(Account::AuthRefresh).unwrap(), None);
}
