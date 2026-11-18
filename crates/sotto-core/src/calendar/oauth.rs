//! Google's OAuth endpoints for an installed app: authorization URL, code exchange, refresh and
//! revoke. Secrets have hand-written `Debug` impls that print nothing sensitive.

use std::time::Duration;

use serde::Deserialize;

use super::error::CalendarError;

pub const SCOPE: &str = "https://www.googleapis.com/auth/calendar.events.readonly";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth_url: String,
    pub token_url: String,
    pub revoke_url: String,
    pub api_base: String,
}

impl Endpoints {
    #[must_use]
    pub fn google() -> Self {
        Self {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            revoke_url: "https://oauth2.googleapis.com/revoke".into(),
            api_base: "https://www.googleapis.com/calendar/v3".into(),
        }
    }
}

#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub secret: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("has_secret", &self.secret.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default = "default_expiry")]
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

fn default_expiry() -> u64 {
    3_600
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    error: Option<String>,
    error_description: Option<String>,
}

pub fn http_client() -> Result<reqwest::Client, CalendarError> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| CalendarError::from_http(&e))
}

pub fn authorization_url(
    endpoints: &Endpoints,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> Result<String, CalendarError> {
    let mut url = url::Url::parse(&endpoints.auth_url)
        .map_err(|_| CalendarError::Browser("invalid authorization URL".into()))?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");
    Ok(url.into())
}

async fn post_form(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, CalendarError> {
    let response = http
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(|e| CalendarError::from_http(&e))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|e| CalendarError::from_http(&e))?;
    if status.is_success() {
        return serde_json::from_slice(&body).map_err(|_| CalendarError::BadResponse);
    }
    let parsed: Option<ErrorBody> = serde_json::from_slice(&body).ok();
    let code = parsed.as_ref().and_then(|b| b.error.as_deref());
    if code == Some("invalid_grant") {
        return Err(CalendarError::GrantRevoked);
    }
    let detail = parsed
        .as_ref()
        .and_then(|b| b.error_description.as_deref().or(b.error.as_deref()))
        .map_or_else(
            || format!("HTTP {}", status.as_u16()),
            |d| d.chars().take(160).collect(),
        );
    Err(CalendarError::Rejected(detail))
}

pub async fn exchange_code(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    client: &Client,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, CalendarError> {
    let mut form = vec![
        ("client_id", client.id.as_str()),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
    ];
    if let Some(secret) = &client.secret {
        form.push(("client_secret", secret));
    }
    post_form(http, &endpoints.token_url, &form).await
}

pub async fn refresh(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    client: &Client,
    refresh_token: &str,
) -> Result<TokenResponse, CalendarError> {
    let mut form = vec![
        ("client_id", client.id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = &client.secret {
        form.push(("client_secret", secret));
    }
    post_form(http, &endpoints.token_url, &form).await
}

pub async fn revoke(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    token: &str,
) -> Result<(), CalendarError> {
    let response = http
        .post(&endpoints.revoke_url)
        .form(&[("token", token)])
        .send()
        .await
        .map_err(|e| CalendarError::from_http(&e))?;
    let status = response.status();
    // 400 means the token was already invalid, which is the state we want.
    if status.is_success() || status == reqwest::StatusCode::BAD_REQUEST {
        Ok(())
    } else {
        Err(CalendarError::Rejected(format!(
            "revoke returned {}",
            status.as_u16()
        )))
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn endpoints(server: &MockServer) -> Endpoints {
        Endpoints {
            auth_url: format!("{}/auth", server.uri()),
            token_url: format!("{}/token", server.uri()),
            revoke_url: format!("{}/revoke", server.uri()),
            api_base: format!("{}/calendar/v3", server.uri()),
        }
    }

    fn client(secret: Option<&str>) -> Client {
        Client {
            id: "cid.apps.googleusercontent.com".into(),
            secret: secret.map(str::to_string),
        }
    }

    #[test]
    fn authorization_url_carries_pkce_state_and_the_narrow_scope() {
        let e = Endpoints::google();
        let u =
            authorization_url(&e, "cid", "http://127.0.0.1:5000/callback", "chal", "st").unwrap();
        let url = url::Url::parse(&u).unwrap();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "chal");
        assert_eq!(q["state"], "st");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5000/callback");
        assert_eq!(q["scope"], SCOPE);
        assert_eq!(q["access_type"], "offline");
        assert!(!u.contains("verifier") && !u.contains("secret"));
    }

    #[tokio::test]
    async fn exchanges_a_code_with_the_verifier_and_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier=ver"))
            .and(body_string_contains("client_secret=shh"))
            .and(body_string_contains("code=c0de"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "expires_in": 3599, "refresh_token": "rt", "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let t = exchange_code(
            &http,
            &endpoints(&server),
            &client(Some("shh")),
            "c0de",
            "ver",
            "http://127.0.0.1:1/callback",
        )
        .await
        .unwrap();
        assert_eq!(t.access_token, "at");
        assert_eq!(t.refresh_token.as_deref(), Some("rt"));
        assert_eq!(t.expires_in, 3599);
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("at\"") && !dbg.contains("rt"));
    }

    #[tokio::test]
    async fn omits_the_secret_when_none_is_stored() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(|req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body).to_string();
                assert!(!body.contains("client_secret"));
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "at"}))
            })
            .expect(1)
            .mount(&server)
            .await;
        let t = refresh(
            &http_client().unwrap(),
            &endpoints(&server),
            &client(None),
            "rt",
        )
        .await
        .unwrap();
        assert_eq!(t.expires_in, 3_600);
    }

    #[tokio::test]
    async fn refresh_maps_invalid_grant_and_other_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("refresh_token=revoked"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({"error": "invalid_grant", "error_description": "Token has been expired or revoked."}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"error": "invalid_client", "error_description": "client_secret is missing."}),
            ))
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let e = endpoints(&server);
        assert!(matches!(
            refresh(&http, &e, &client(None), "revoked").await,
            Err(CalendarError::GrantRevoked)
        ));
        match refresh(&http, &e, &client(None), "other").await {
            Err(CalendarError::Rejected(m)) => assert_eq!(m, "client_secret is missing."),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn revoke_treats_an_already_invalid_token_as_done() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .and(body_string_contains("token=rt"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .and(body_string_contains("token=old"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let http = http_client().unwrap();
        let e = endpoints(&server);
        assert!(revoke(&http, &e, "rt").await.is_ok());
        assert!(revoke(&http, &e, "old").await.is_ok());
        assert!(revoke(&http, &e, "boom").await.is_err());
    }

    #[test]
    fn client_debug_hides_id_and_secret() {
        let text = format!("{:?}", client(Some("topsecret")));
        assert!(!text.contains("topsecret") && !text.contains("cid"));
    }
}
