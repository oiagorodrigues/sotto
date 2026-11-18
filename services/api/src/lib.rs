//! Sotto backend: accounts, device login, preference sync and ephemeral provider tokens.
//! It never receives meeting content (ADR 0002).

pub mod config;
pub mod crypto;
pub mod error;
pub mod mail;
pub mod oauth;
pub mod pages;
pub mod prefs;
pub mod ratelimit;
pub mod realtime;
#[cfg(test)]
mod tests;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        ConnectInfo, DefaultBodyLimit, FromRef, FromRequest, FromRequestParts, MatchedPath, Request,
    },
    http::{HeaderName, HeaderValue, StatusCode, header, request::Parts},
    routing::{get, post},
};
use serde::de::DeserializeOwned;
use sotto_protocol::api::{HealthResponse, MeResponse};
use sqlx::PgPool;
use tower_http::{set_header::SetResponseHeaderLayer, timeout::TimeoutLayer, trace::TraceLayer};
use uuid::Uuid;

use crate::{config::Config, error::ApiError, mail::Mailer, ratelimit::RateLimiter};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub mailer: Arc<dyn Mailer>,
    pub limiter: Arc<RateLimiter>,
    pub http: reqwest::Client,
    /// Verified against when an email is unknown, so a miss costs as much as a wrong password.
    pub dummy_hash: Arc<String>,
}

impl AppState {
    pub fn new(db: PgPool, config: Config, mailer: Arc<dyn Mailer>) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(ApiError::internal)?;
        let random = crypto::random_token().map_err(ApiError::internal)?;
        let dummy_hash = crypto::hash_password(&random).map_err(ApiError::internal)?;
        Ok(Self {
            db,
            config: Arc::new(config),
            mailer,
            limiter: Arc::new(RateLimiter::new()),
            http,
            dummy_hash: Arc::new(dummy_hash),
        })
    }
}

impl FromRef<AppState> for PgPool {
    fn from_ref(s: &AppState) -> Self {
        s.db.clone()
    }
}

/// Peer address from the connection. Behind a proxy this is the proxy until a trusted
/// forwarded header is supported.
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.ip());
        Ok(Self(ip))
    }
}

/// JSON body with the API's own error format and the route's body limit.
#[derive(Debug)]
pub struct ApiJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let json_type = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"));
        if !json_type {
            return Err(ApiError::InvalidRequest(
                "Content-Type must be application/json.".into(),
            ));
        }
        let bytes = Bytes::from_request(req, state).await.map_err(|e| {
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::PayloadTooLarge
            } else {
                ApiError::InvalidRequest("Could not read the request body.".into())
            }
        })?;
        // serde errors quote offending values, so only a generic message goes back and
        // nothing is logged.
        serde_json::from_slice(&bytes).map(Self).map_err(|_| {
            ApiError::InvalidRequest("The request body does not match the schema.".into())
        })
    }
}

/// The account behind a valid bearer access token.
#[derive(Debug, Clone, Copy)]
pub struct Authed {
    pub account_id: Uuid,
    pub family_id: Uuid,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|t| !t.is_empty() && t.len() <= 256)
            .ok_or(ApiError::Unauthorized)?;
        let row: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT f.account_id, f.id FROM access_tokens t \
             JOIN token_families f ON f.id = t.family_id \
             WHERE t.token_hash = $1 AND t.expires_at > now() AND f.revoked_at IS NULL",
        )
        .bind(crypto::hash_token(token))
        .fetch_optional(&state.db)
        .await?;
        row.map(|(account_id, family_id)| Self {
            account_id,
            family_id,
        })
        .ok_or(ApiError::Unauthorized)
    }
}

pub fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
    })
}

async fn me(
    State(state): axum::extract::State<AppState>,
    auth: Authed,
) -> Result<Json<MeResponse>, ApiError> {
    let (email, created_at): (String, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT email, created_at FROM accounts WHERE id = $1")
            .bind(auth.account_id)
            .fetch_one(&state.db)
            .await?;
    Ok(Json(MeResponse {
        id: auth.account_id.to_string(),
        email,
        created_at: rfc3339(created_at),
    }))
}

use axum::extract::State;

const CSP: &str =
    "default-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'";

pub fn router(state: AppState) -> Router {
    let prefs_routes = get(prefs::get_preferences)
        .put(prefs::put_preferences)
        .layer(DefaultBodyLimit::max(
            sotto_protocol::api::MAX_PREFERENCES_BODY_BYTES,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/static/auth.css", get(pages::stylesheet))
        .route("/authorize", get(oauth::authorize))
        .route("/authorize/signin", post(oauth::sign_in))
        .route("/authorize/signup", post(oauth::sign_up))
        .route("/authorize/magic", post(oauth::request_magic_link))
        .route("/magic/{token}", get(oauth::consume_magic_link))
        .route("/v1/oauth/token", post(oauth::token))
        .route("/v1/logout", post(oauth::logout))
        .route("/v1/me", get(me))
        .route("/v1/preferences", prefs_routes)
        .route("/v1/realtime-tokens", post(realtime::mint))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(15),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request| {
                    let route = req
                        .extensions()
                        .get::<MatchedPath>()
                        .map_or("unmatched", MatchedPath::as_str);
                    tracing::info_span!("http", method = %req.method(), route)
                })
                .on_request(())
                .on_response(
                    |res: &axum::response::Response, latency: Duration, _span: &tracing::Span| {
                        tracing::info!(
                            status = res.status().as_u16(),
                            latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
                            "request"
                        );
                    },
                ),
        )
        .with_state(state)
}

/// Deletes rows that can no longer be used. Families, accounts and preferences stay.
pub async fn purge_expired(db: &PgPool) -> Result<(), sqlx::Error> {
    for sql in [
        "DELETE FROM magic_links WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM auth_codes WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM access_tokens WHERE expires_at < now()",
        "DELETE FROM refresh_tokens WHERE expires_at < now() - interval '1 day'",
        "DELETE FROM realtime_token_audit WHERE created_at < now() - interval '90 days'",
    ] {
        sqlx::query(sql).execute(db).await?;
    }
    Ok(())
}
