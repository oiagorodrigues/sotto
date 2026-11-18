use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sotto_protocol::api::{ErrorBody, PreferencesConflict, PreferencesDocument};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid grant")]
    InvalidGrant,
    #[error("unauthorized")]
    Unauthorized,
    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },
    #[error("conflict")]
    Conflict(Box<PreferencesDocument>),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("upstream unavailable")]
    Upstream,
    #[error("internal error")]
    Internal,
}

impl ApiError {
    /// Logs the cause and hides it from the client. The cause is a database or crypto error
    /// message, which never includes request bodies.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(cause = %cause, "internal error");
        Self::Internal
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::internal(e)
    }
}

fn body(error: &str, message: &str) -> ErrorBody {
    ErrorBody {
        error: error.to_owned(),
        message: message.to_owned(),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidRequest(m) => {
                (StatusCode::BAD_REQUEST, Json(body("invalid_request", &m))).into_response()
            }
            Self::InvalidGrant => (
                StatusCode::BAD_REQUEST,
                Json(body(
                    "invalid_grant",
                    "The grant is invalid, expired or already used.",
                )),
            )
                .into_response(),
            Self::Unauthorized => {
                let mut r = (
                    StatusCode::UNAUTHORIZED,
                    Json(body("unauthorized", "A valid access token is required.")),
                )
                    .into_response();
                r.headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                r
            }
            Self::RateLimited { retry_after_secs } => {
                let mut r = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(body("rate_limited", "Too many requests. Try again later.")),
                )
                    .into_response();
                r.headers_mut().insert(
                    header::RETRY_AFTER,
                    HeaderValue::from(retry_after_secs.max(1)),
                );
                r
            }
            Self::Conflict(current) => (
                StatusCode::CONFLICT,
                Json(PreferencesConflict {
                    error: "conflict".to_owned(),
                    message: "The preferences changed on the server. Merge and retry.".to_owned(),
                    current: *current,
                }),
            )
                .into_response(),
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(body("payload_too_large", "The request body is too large.")),
            )
                .into_response(),
            Self::Upstream => (
                StatusCode::BAD_GATEWAY,
                Json(body(
                    "upstream_unavailable",
                    "The provider could not issue a token.",
                )),
            )
                .into_response(),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(body("internal", "Something went wrong.")),
            )
                .into_response(),
        }
    }
}
