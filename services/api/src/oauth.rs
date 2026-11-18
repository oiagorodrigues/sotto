//! Device login (ADR 0008): the browser login pages, the magic link, and the token endpoint.

use std::time::Duration;

use axum::{
    Form, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use sotto_protocol::api::{
    ACCESS_TOKEN_TTL_SECS, AUTH_CODE_TTL_SECS, CLIENT_ID, REDIRECT_URI, REFRESH_TOKEN_TTL_SECS,
    TokenRequest, TokenResponse,
};
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use crate::{ApiJson, AppState, Authed, ClientIp, crypto, error::ApiError, mail::Mail, pages};

const CSRF_COOKIE: &str = "sotto_csrf";
const MAGIC_LINK_TTL_MINUTES: i32 = 15;
const SIGNIN_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAIL_WINDOW: Duration = Duration::from_secs(3600);
const TOKEN_WINDOW: Duration = Duration::from_secs(60);

/// The authorize request, carried through the login forms as hidden fields and checked again
/// on every post.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthParams {
    #[serde(default)]
    pub response_type: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub redirect_uri: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub code_challenge: String,
    #[serde(default)]
    pub code_challenge_method: String,
}

fn is_unreserved(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')
}

impl AuthParams {
    fn validate(&self) -> Result<(), &'static str> {
        if self.response_type != "code" {
            return Err("Unsupported response type.");
        }
        if self.client_id != CLIENT_ID {
            return Err("Unknown client.");
        }
        if self.redirect_uri != REDIRECT_URI {
            return Err("The redirect address is not allowed.");
        }
        if self.code_challenge_method != "S256" {
            return Err("Only the S256 code challenge method is supported.");
        }
        if self.state.is_empty() || self.state.len() > 128 || !self.state.chars().all(is_unreserved)
        {
            return Err("The state value is invalid.");
        }
        let challenge_ok = self.code_challenge.len() == 43
            && self
                .code_challenge
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !challenge_ok {
            return Err("The code challenge is invalid.");
        }
        Ok(())
    }
}

fn html(status: StatusCode, body: String) -> Response {
    (status, Html(body)).into_response()
}

fn bad_request(message: &str) -> Response {
    html(
        StatusCode::BAD_REQUEST,
        pages::message_page("This sign-in link cannot be used", message),
    )
}

fn rate_limited_page(retry_after_secs: u64) -> Response {
    let mut r = html(
        StatusCode::TOO_MANY_REQUESTS,
        pages::message_page("Too many attempts", "Wait a few minutes and try again."),
    );
    r.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(retry_after_secs.max(1)),
    );
    r
}

fn server_error_page() -> Response {
    html(
        StatusCode::INTERNAL_SERVER_ERROR,
        pages::message_page("Something went wrong", "Try again in a moment."),
    )
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn csrf_cookie(state: &AppState, token: &str) -> HeaderValue {
    let secure = if state.config.public_base_url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    // Token is base64url, so the value is always a valid header.
    HeaderValue::from_str(&format!(
        "{CSRF_COOKIE}={token}; Path=/authorize; HttpOnly; SameSite=Strict; Max-Age=1800{secure}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

pub async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<AuthParams>,
) -> Response {
    if let Err(m) = params.validate() {
        return bad_request(m);
    }
    let existing = cookie_value(&headers, CSRF_COOKIE).filter(|t| t.len() == 43);
    let token = match existing {
        Some(t) => t.to_owned(),
        None => match crypto::random_token() {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(cause = %e, "csrf token");
                return server_error_page();
            }
        },
    };
    let mut r = html(StatusCode::OK, pages::login_page(&params, &token, None));
    r.headers_mut()
        .insert(header::SET_COOKIE, csrf_cookie(&state, &token));
    r
}

#[derive(Deserialize)]
pub struct CredentialsForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(flatten)]
    params: AuthParams,
}

#[derive(Deserialize)]
pub struct EmailForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    email: String,
    #[serde(flatten)]
    params: AuthParams,
}

impl std::fmt::Debug for CredentialsForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialsForm").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for EmailForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailForm").finish_non_exhaustive()
    }
}

fn check_csrf(headers: &HeaderMap, field: &str) -> Result<(), Box<Response>> {
    match cookie_value(headers, CSRF_COOKIE) {
        Some(c) if !c.is_empty() && crypto::ct_eq(c, field) => Ok(()),
        _ => Err(Box::new(html(
            StatusCode::FORBIDDEN,
            pages::message_page(
                "Session expired",
                "Go back to Sotto and start the sign-in again.",
            ),
        ))),
    }
}

fn check_limit(
    state: &AppState,
    key: String,
    max: u32,
    window: Duration,
) -> Result<(), Box<Response>> {
    state
        .limiter
        .check(&key, max, window)
        .map_err(|secs| Box::new(rate_limited_page(secs)))
}

pub fn normalize_email(raw: &str) -> Option<String> {
    let e = raw.trim().to_lowercase();
    if e.is_empty() || e.len() > 254 {
        return None;
    }
    if e.chars().any(|c| {
        c.is_whitespace()
            || c.is_control()
            || matches!(
                c,
                '<' | '>' | ',' | ';' | '"' | '\\' | '(' | ')' | '[' | ']'
            )
    }) {
        return None;
    }
    let (local, domain) = e.split_once('@')?;
    let domain_ok = domain.contains('.')
        && !domain.contains('@')
        && !domain.starts_with(['.', '-'])
        && !domain.ends_with(['.', '-']);
    (!local.is_empty() && local.len() <= 64 && domain_ok).then_some(e)
}

fn password_ok(p: &str) -> bool {
    (10..=128).contains(&p.chars().count())
}

async fn hash_blocking(password: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || crypto::hash_password(&password))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)
}

async fn verify_blocking(password: String, stored: String) -> Result<bool, ApiError> {
    tokio::task::spawn_blocking(move || crypto::verify_password(&password, &stored))
        .await
        .map_err(ApiError::internal)
}

async fn create_auth_code(
    db: impl PgExecutor<'_>,
    account_id: Uuid,
    p: &AuthParams,
) -> Result<String, ApiError> {
    let code = crypto::random_token().map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO auth_codes (code_hash, account_id, client_id, redirect_uri, code_challenge, expires_at) \
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))",
    )
    .bind(crypto::hash_token(&code))
    .bind(account_id)
    .bind(&p.client_id)
    .bind(&p.redirect_uri)
    .bind(&p.code_challenge)
    .bind(AUTH_CODE_TTL_SECS as f64)
    .execute(db)
    .await?;
    Ok(code)
}

fn handoff(code: &str, state: &str) -> Response {
    // `state` passed validation, so it is made of URL-safe characters only.
    let url = format!("{REDIRECT_URI}?code={code}&state={state}");
    html(StatusCode::OK, pages::handoff_page(&url))
}

pub async fn sign_in(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Form(f): Form<CredentialsForm>,
) -> Response {
    if let Err(r) = check_csrf(&headers, &f.csrf) {
        return *r;
    }
    if let Err(m) = f.params.validate() {
        return bad_request(m);
    }
    let lim = &state.config.limits;
    if let Err(r) = check_limit(
        &state,
        format!("signin:ip:{ip}"),
        lim.signin_per_ip,
        SIGNIN_WINDOW,
    ) {
        return *r;
    }
    let email = normalize_email(&f.email);
    if let Some(e) = &email
        && let Err(r) = check_limit(
            &state,
            format!("signin:email:{e}"),
            lim.signin_per_email,
            SIGNIN_WINDOW,
        )
    {
        return *r;
    }
    match try_sign_in(&state, email, f.password, &f.params).await {
        Ok(Some(code)) => handoff(&code, &f.params.state),
        Ok(None) => html(
            StatusCode::UNAUTHORIZED,
            pages::login_page(&f.params, &f.csrf, Some("The email or password is wrong.")),
        ),
        Err(_) => server_error_page(),
    }
}

async fn try_sign_in(
    state: &AppState,
    email: Option<String>,
    password: String,
    params: &AuthParams,
) -> Result<Option<String>, ApiError> {
    let account: Option<(Uuid, Option<String>)> = match &email {
        Some(e) => {
            sqlx::query_as("SELECT id, password_hash FROM accounts WHERE email = $1")
                .bind(e)
                .fetch_optional(&state.db)
                .await?
        }
        None => None,
    };
    let (account_id, stored) = match account {
        Some((id, Some(hash))) => (Some(id), hash),
        _ => (None, state.dummy_hash.as_str().to_owned()),
    };
    let verified = verify_blocking(password, stored).await?;
    match account_id {
        Some(id) if verified => Ok(Some(create_auth_code(&state.db, id, params).await?)),
        _ => Ok(None),
    }
}

async fn send_link(
    state: &AppState,
    email: &str,
    password_hash: Option<String>,
    params: &AuthParams,
) -> Result<(), ApiError> {
    let token = crypto::random_token().map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO magic_links (token_hash, email, password_hash, client_id, redirect_uri, state, code_challenge, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(mins => $8))",
    )
    .bind(crypto::hash_token(&token))
    .bind(email)
    .bind(password_hash)
    .bind(&params.client_id)
    .bind(&params.redirect_uri)
    .bind(&params.state)
    .bind(&params.code_challenge)
    .bind(MAGIC_LINK_TTL_MINUTES)
    .execute(&state.db)
    .await?;
    let mail = Mail {
        to: email.to_owned(),
        subject: "Your Sotto sign-in link".to_owned(),
        body: format!(
            "Open this link to continue signing in to Sotto:\n\n{}/magic/{token}\n\n\
             It works once and expires in {MAGIC_LINK_TTL_MINUTES} minutes. If you did not ask for it, ignore this email.\n",
            state.config.public_base_url
        ),
    };
    // A delivery failure must look the same to the visitor as a success, or the response
    // would tell whether the address is deliverable.
    if state.mailer.send(mail).await.is_err() {
        tracing::warn!("magic link delivery failed");
    }
    Ok(())
}

pub async fn request_magic_link(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Form(f): Form<EmailForm>,
) -> Response {
    if let Err(r) = check_csrf(&headers, &f.csrf) {
        return *r;
    }
    if let Err(m) = f.params.validate() {
        return bad_request(m);
    }
    let Some(email) = normalize_email(&f.email) else {
        return html(
            StatusCode::BAD_REQUEST,
            pages::login_page(&f.params, &f.csrf, Some("Enter a valid email address.")),
        );
    };
    let lim = &state.config.limits;
    if let Err(r) = check_limit(
        &state,
        format!("mail:ip:{ip}"),
        lim.mail_per_ip,
        MAIL_WINDOW,
    ) {
        return *r;
    }
    if let Err(r) = check_limit(
        &state,
        format!("mail:email:{email}"),
        lim.mail_per_email,
        MAIL_WINDOW,
    ) {
        return *r;
    }
    match send_link(&state, &email, None, &f.params).await {
        Ok(()) => html(StatusCode::OK, pages::check_email_page()),
        Err(_) => server_error_page(),
    }
}

/// Sign-up is a mailed link too, so the response is the same whether or not the address has an
/// account. The password is hashed in both cases and only used if the link creates the account.
pub async fn sign_up(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Form(f): Form<CredentialsForm>,
) -> Response {
    if let Err(r) = check_csrf(&headers, &f.csrf) {
        return *r;
    }
    if let Err(m) = f.params.validate() {
        return bad_request(m);
    }
    let Some(email) = normalize_email(&f.email) else {
        return html(
            StatusCode::BAD_REQUEST,
            pages::login_page(&f.params, &f.csrf, Some("Enter a valid email address.")),
        );
    };
    if !password_ok(&f.password) {
        return html(
            StatusCode::BAD_REQUEST,
            pages::login_page(
                &f.params,
                &f.csrf,
                Some("Use a password of 10 to 128 characters."),
            ),
        );
    }
    let lim = &state.config.limits;
    if let Err(r) = check_limit(
        &state,
        format!("mail:ip:{ip}"),
        lim.mail_per_ip,
        MAIL_WINDOW,
    ) {
        return *r;
    }
    if let Err(r) = check_limit(
        &state,
        format!("mail:email:{email}"),
        lim.mail_per_email,
        MAIL_WINDOW,
    ) {
        return *r;
    }
    let result = async {
        let hash = hash_blocking(f.password).await?;
        send_link(&state, &email, Some(hash), &f.params).await
    }
    .await;
    match result {
        Ok(()) => html(StatusCode::OK, pages::check_email_page()),
        Err(_) => server_error_page(),
    }
}

#[derive(sqlx::FromRow)]
struct ConsumedLink {
    email: String,
    password_hash: Option<String>,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
}

pub async fn consume_magic_link(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Path(token): Path<String>,
) -> Response {
    if let Err(r) = check_limit(
        &state,
        format!("token:ip:{ip}"),
        state.config.limits.token_per_ip,
        TOKEN_WINDOW,
    ) {
        return *r;
    }
    let invalid = || {
        bad_request(
            "This link is invalid, expired or already used. Start the sign-in again from Sotto.",
        )
    };
    if token.len() != 43 {
        return invalid();
    }
    match finish_magic_link(&state, &token).await {
        Ok(Some((code, params))) => handoff(&code, &params.state),
        Ok(None) => invalid(),
        Err(_) => server_error_page(),
    }
}

async fn finish_magic_link(
    state: &AppState,
    token: &str,
) -> Result<Option<(String, AuthParams)>, ApiError> {
    let mut tx = state.db.begin().await?;
    let link: Option<ConsumedLink> = sqlx::query_as(
        "UPDATE magic_links SET used_at = now() \
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now() \
         RETURNING email, password_hash, client_id, redirect_uri, state, code_challenge",
    )
    .bind(crypto::hash_token(token))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(link) = link else {
        return Ok(None);
    };
    // An existing account keeps its password: the hash on the link only seeds new accounts.
    let (account_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO accounts (id, email, password_hash) VALUES ($1, $2, $3) \
         ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(&link.email)
    .bind(&link.password_hash)
    .fetch_one(&mut *tx)
    .await?;
    let params = AuthParams {
        response_type: "code".to_owned(),
        client_id: link.client_id,
        redirect_uri: link.redirect_uri,
        state: link.state,
        code_challenge: link.code_challenge,
        code_challenge_method: "S256".to_owned(),
    };
    let code = create_auth_code(&mut *tx, account_id, &params).await?;
    tx.commit().await?;
    Ok(Some((code, params)))
}

async fn revoke_family(db: impl PgExecutor<'_>, family_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE token_families SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(family_id)
    .execute(db)
    .await?;
    Ok(())
}

async fn issue_tokens(conn: &mut PgConnection, family_id: Uuid) -> Result<TokenResponse, ApiError> {
    let access = crypto::random_token().map_err(ApiError::internal)?;
    let refresh = crypto::random_token().map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO access_tokens (token_hash, family_id, expires_at) \
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(crypto::hash_token(&access))
    .bind(family_id)
    .bind(ACCESS_TOKEN_TTL_SECS as f64)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO refresh_tokens (token_hash, family_id, expires_at) \
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(crypto::hash_token(&refresh))
    .bind(family_id)
    .bind(REFRESH_TOKEN_TTL_SECS as f64)
    .execute(&mut *conn)
    .await?;
    Ok(TokenResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".to_owned(),
        expires_in: ACCESS_TOKEN_TTL_SECS,
    })
}

fn valid_verifier(v: &str) -> bool {
    (43..=128).contains(&v.len()) && v.chars().all(is_unreserved)
}

pub async fn token(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<TokenRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    state
        .limiter
        .check(
            &format!("token:ip:{ip}"),
            state.config.limits.token_per_ip,
            TOKEN_WINDOW,
        )
        .map_err(|retry_after_secs| ApiError::RateLimited { retry_after_secs })?;
    let resp = match req {
        TokenRequest::AuthorizationCode {
            code,
            code_verifier,
            redirect_uri,
            client_id,
        } => exchange_code(&state, &code, &code_verifier, &redirect_uri, &client_id).await?,
        TokenRequest::RefreshToken {
            refresh_token,
            client_id,
        } => {
            if client_id != CLIENT_ID {
                return Err(ApiError::InvalidGrant);
            }
            rotate_refresh(&state, &refresh_token).await?
        }
    };
    Ok(Json(resp))
}

async fn exchange_code(
    state: &AppState,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    client_id: &str,
) -> Result<TokenResponse, ApiError> {
    if client_id != CLIENT_ID
        || redirect_uri != REDIRECT_URI
        || !valid_verifier(verifier)
        || code.len() != 43
    {
        return Err(ApiError::InvalidGrant);
    }
    let code_hash = crypto::hash_token(code);
    // The code is spent before anything else is checked, so a wrong verifier burns it.
    let row: Option<(Uuid, String, String, String)> = sqlx::query_as(
        "UPDATE auth_codes SET used_at = now() \
         WHERE code_hash = $1 AND used_at IS NULL AND expires_at > now() \
         RETURNING account_id, client_id, redirect_uri, code_challenge",
    )
    .bind(&code_hash)
    .fetch_optional(&state.db)
    .await?;
    let Some((account_id, bound_client, bound_redirect, challenge)) = row else {
        let earlier: Option<(Option<Uuid>,)> = sqlx::query_as(
            "SELECT family_id FROM auth_codes WHERE code_hash = $1 AND used_at IS NOT NULL",
        )
        .bind(&code_hash)
        .fetch_optional(&state.db)
        .await?;
        if let Some((Some(family),)) = earlier {
            revoke_family(&state.db, family).await?;
        }
        return Err(ApiError::InvalidGrant);
    };
    let matches = crypto::ct_eq(&crypto::pkce_s256(verifier), &challenge)
        && bound_client == client_id
        && bound_redirect == redirect_uri;
    if !matches {
        return Err(ApiError::InvalidGrant);
    }
    let family_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO token_families (id, account_id) VALUES ($1, $2)")
        .bind(family_id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE auth_codes SET family_id = $2 WHERE code_hash = $1")
        .bind(&code_hash)
        .bind(family_id)
        .execute(&mut *tx)
        .await?;
    let resp = issue_tokens(&mut tx, family_id).await?;
    tx.commit().await?;
    Ok(resp)
}

async fn rotate_refresh(state: &AppState, refresh_token: &str) -> Result<TokenResponse, ApiError> {
    if refresh_token.len() != 43 {
        return Err(ApiError::InvalidGrant);
    }
    let hash = crypto::hash_token(refresh_token);
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE refresh_tokens SET used_at = now() \
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now() RETURNING family_id",
    )
    .bind(&hash)
    .fetch_optional(&state.db)
    .await?;
    let Some((family_id,)) = row else {
        let reused: Option<(Uuid,)> = sqlx::query_as(
            "SELECT family_id FROM refresh_tokens WHERE token_hash = $1 AND used_at IS NOT NULL",
        )
        .bind(&hash)
        .fetch_optional(&state.db)
        .await?;
        if let Some((family,)) = reused {
            tracing::warn!("refresh token reuse, family revoked");
            revoke_family(&state.db, family).await?;
        }
        return Err(ApiError::InvalidGrant);
    };
    let mut tx = state.db.begin().await?;
    let live: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM token_families WHERE id = $1 AND revoked_at IS NULL FOR SHARE",
    )
    .bind(family_id)
    .fetch_optional(&mut *tx)
    .await?;
    if live.is_none() {
        return Err(ApiError::InvalidGrant);
    }
    let resp = issue_tokens(&mut tx, family_id).await?;
    tx.commit().await?;
    Ok(resp)
}

pub async fn logout(State(state): State<AppState>, auth: Authed) -> Result<StatusCode, ApiError> {
    revoke_family(&state.db, auth.family_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_normalization() {
        assert_eq!(
            normalize_email("  Ada@Example.COM "),
            Some("ada@example.com".into())
        );
        for bad in [
            "",
            "a",
            "a@b",
            "a b@c.com",
            "@c.com",
            "a@@c.com",
            "a@c.com\r\nbcc:x@y.z",
            "a@.com",
            "<a>@c.com",
        ] {
            assert_eq!(normalize_email(bad), None, "{bad}");
        }
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; sotto_csrf=abc; b=2"),
        );
        assert_eq!(cookie_value(&h, "sotto_csrf"), Some("abc"));
        assert_eq!(cookie_value(&h, "missing"), None);
    }
}
