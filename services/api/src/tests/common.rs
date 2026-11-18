use std::{net::SocketAddr, sync::Arc};

use crate::{
    AppState,
    config::{Config, Limits, RealtimeConfig},
    crypto,
    mail::{CaptureMailer, Mail},
    router,
};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

pub const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
pub const STATE: &str = "state-123.abc_~";
pub const REDIRECT: &str = "sotto://auth/callback";
pub const PASSWORD: &str = "correct horse battery";

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Resp {
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("bad json {e}: {}", self.body))
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    /// The `sotto://` URL of the handoff page, if the body carries one.
    pub fn callback_url(&self) -> Option<String> {
        let start = self.body.find("sotto://auth/callback?")?;
        let rest = &self.body[start..];
        let end = rest.find('"')?;
        Some(rest[..end].replace("&amp;", "&"))
    }
}

pub struct App {
    pub router: Router,
    pub mailer: Arc<CaptureMailer>,
    pub pool: PgPool,
}

pub fn config(realtime: RealtimeConfig, limits: Limits) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        public_base_url: "http://sotto.test".to_owned(),
        mail_from: "Sotto <login@sotto.test>".to_owned(),
        realtime,
        limits,
    }
}

pub fn high_limits() -> Limits {
    Limits {
        signin_per_ip: 1000,
        signin_per_email: 1000,
        mail_per_email: 1000,
        mail_per_ip: 1000,
        token_per_ip: 1000,
        realtime_per_account: 20,
    }
}

impl App {
    pub fn new(pool: PgPool) -> Self {
        Self::with(pool, RealtimeConfig::Fake, high_limits())
    }

    pub fn with(pool: PgPool, realtime: RealtimeConfig, limits: Limits) -> Self {
        let mailer = Arc::new(CaptureMailer::default());
        let state = AppState::new(pool.clone(), config(realtime, limits), mailer.clone()).unwrap();
        Self {
            router: router(state),
            mailer,
            pool,
        }
    }

    pub async fn send(&self, req: Request<Body>) -> Resp {
        self.send_from(req, "10.0.0.1:5000").await
    }

    pub async fn send_from(&self, mut req: Request<Body>, peer: &str) -> Resp {
        let addr: SocketAddr = peer.parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        let res = self.router.clone().oneshot(req).await.unwrap();
        let (parts, body) = res.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        Resp {
            status: parts.status,
            headers: parts.headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    pub async fn get(&self, uri: &str) -> Resp {
        self.send(Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    pub async fn get_auth(&self, uri: &str, token: &str) -> Resp {
        self.send(
            Request::get(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn json(
        &self,
        method: Method,
        uri: &str,
        bearer: Option<&str>,
        body: &Value,
    ) -> Resp {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = bearer {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        self.send(b.body(Body::from(body.to_string())).unwrap())
            .await
    }

    pub async fn post_form(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        csrf_cookie: Option<&str>,
    ) -> Resp {
        self.post_form_from(path, fields, csrf_cookie, "10.0.0.1:5000")
            .await
    }

    pub async fn post_form_from(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        csrf_cookie: Option<&str>,
        peer: &str,
    ) -> Resp {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
            .collect::<Vec<_>>()
            .join("&");
        let mut b =
            Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(c) = csrf_cookie {
            b = b.header(header::COOKIE, format!("sotto_csrf={c}"));
        }
        self.send_from(b.body(Body::from(body)).unwrap(), peer)
            .await
    }

    pub fn challenge() -> String {
        crypto::pkce_s256(VERIFIER)
    }

    pub fn authorize_uri(state: &str, challenge: &str) -> String {
        format!(
            "/authorize?response_type=code&client_id=sotto-desktop&redirect_uri={}&state={state}&code_challenge={challenge}&code_challenge_method=S256",
            enc(REDIRECT)
        )
    }

    /// Opens the login page and returns the CSRF token from its cookie.
    pub async fn open_login(&self) -> String {
        let r = self
            .get(&Self::authorize_uri(STATE, &Self::challenge()))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let cookie = r.header("set-cookie");
        let value = cookie
            .split(';')
            .next()
            .unwrap()
            .strip_prefix("sotto_csrf=")
            .unwrap();
        assert_eq!(value.len(), 43);
        value.to_owned()
    }

    pub fn fields<'a>(csrf: &'a str, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, String)> {
        let mut v: Vec<(&str, String)> = vec![
            ("csrf", csrf.to_owned()),
            ("response_type", "code".into()),
            ("client_id", "sotto-desktop".into()),
            ("redirect_uri", REDIRECT.into()),
            ("state", STATE.into()),
            ("code_challenge", Self::challenge()),
            ("code_challenge_method", "S256".into()),
        ];
        v.extend(extra.iter().map(|(k, val)| (*k, (*val).to_owned())));
        v
    }

    pub async fn login_post(&self, path: &str, csrf: &str, extra: &[(&str, &str)]) -> Resp {
        let owned = Self::fields(csrf, extra);
        let refs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.post_form(path, &refs, Some(csrf)).await
    }

    pub fn take_mail(&self) -> Vec<Mail> {
        self.mailer.take()
    }

    /// Creates an account through the mailed sign-up link and returns the auth code.
    pub async fn sign_up(&self, email: &str, password: &str) -> String {
        let csrf = self.open_login().await;
        let r = self
            .login_post(
                "/authorize/signup",
                &csrf,
                &[("email", email), ("password", password)],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let mail = self.take_mail().pop().expect("mail sent");
        self.follow_magic(&mail).await
    }

    pub fn magic_path(mail: &Mail) -> String {
        let start = mail.body.find("http://sotto.test").unwrap();
        let rest = &mail.body[start + "http://sotto.test".len()..];
        rest.split_whitespace().next().unwrap().to_owned()
    }

    pub async fn follow_magic(&self, mail: &Mail) -> String {
        let r = self.get(&Self::magic_path(mail)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        code_from(&r.callback_url().expect("handoff url"))
    }

    pub async fn password_login_code(&self, email: &str, password: &str) -> Resp {
        let csrf = self.open_login().await;
        self.login_post(
            "/authorize/signin",
            &csrf,
            &[("email", email), ("password", password)],
        )
        .await
    }

    pub async fn exchange(&self, code: &str) -> Resp {
        self.json(
            Method::POST,
            "/v1/oauth/token",
            None,
            &json!({"grant_type":"authorization_code","code":code,"code_verifier":VERIFIER,"redirect_uri":REDIRECT,"client_id":"sotto-desktop"}),
        )
        .await
    }

    pub async fn refresh(&self, refresh_token: &str) -> Resp {
        self.json(
            Method::POST,
            "/v1/oauth/token",
            None,
            &json!({"grant_type":"refresh_token","refresh_token":refresh_token,"client_id":"sotto-desktop"}),
        )
        .await
    }

    /// Full device login for a new account: returns (access, refresh).
    pub async fn tokens_for(&self, email: &str) -> (String, String) {
        let code = self.sign_up(email, PASSWORD).await;
        let r = self.exchange(&code).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let v: Value = r.json();
        (
            v["access_token"].as_str().unwrap().to_owned(),
            v["refresh_token"].as_str().unwrap().to_owned(),
        )
    }
}

pub fn code_from(url: &str) -> String {
    let q = url.split_once('?').unwrap().1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("code="))
        .unwrap()
        .to_owned()
}

pub fn query_param(url: &str, name: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
        .map(str::to_owned)
}

pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
