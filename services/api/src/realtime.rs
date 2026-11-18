use axum::{Json, extract::State};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use sotto_protocol::api::{RealtimeProvider, RealtimeTokenRequest, RealtimeTokenResponse};

use crate::{ApiJson, AppState, Authed, config::RealtimeConfig, crypto, error::ApiError, rfc3339};

const TOKEN_TTL_SECS: i64 = 600;
const AUDIT_ACTION: &str = "mint_realtime_token";

/// `en` from `en-US`, only when it is a plain 2 letter language code. Chinese is left out
/// because the transcription API wants regional `zh-*` codes for it.
fn language_hint(locale: Option<&str>) -> Option<String> {
    let lang = locale?.split(['-', '_']).next()?.to_ascii_lowercase();
    (lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase()) && lang != "zh")
        .then_some(lang)
}

#[derive(Deserialize)]
struct ClientSecret {
    value: String,
    expires_at: i64,
}

async fn mint_openai(
    state: &AppState,
    api_key: &str,
    base_url: &str,
    model: &str,
    locale: Option<&str>,
) -> Result<(String, DateTime<Utc>), ApiError> {
    let mut transcription = json!({ "model": model });
    if let Some(lang) = language_hint(locale) {
        // `gpt-live-transcribe` takes `languages`; the singular field is for other models.
        transcription["languages"] = json!([lang]);
    }
    let body = json!({
        "expires_after": { "anchor": "created_at", "seconds": TOKEN_TTL_SECS },
        "session": {
            "type": "transcription",
            "audio": { "input": {
                "format": { "type": "audio/pcm", "rate": 24000 },
                "transcription": transcription,
                "turn_detection": null,
            } },
        },
    });
    let url = format!(
        "{}/v1/realtime/client_secrets",
        base_url.trim_end_matches('/')
    );
    let resp = state
        .http
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(timeout = e.is_timeout(), "realtime provider request failed");
            ApiError::Upstream
        })?;
    if !resp.status().is_success() {
        tracing::error!(
            status = resp.status().as_u16(),
            "realtime provider rejected the request"
        );
        return Err(ApiError::Upstream);
    }
    let secret: ClientSecret = resp.json().await.map_err(|_| {
        tracing::error!("realtime provider returned an unexpected body");
        ApiError::Upstream
    })?;
    let expires = DateTime::from_timestamp(secret.expires_at, 0).ok_or(ApiError::Upstream)?;
    Ok((secret.value, expires))
}

pub async fn mint(
    State(state): State<AppState>,
    auth: Authed,
    ApiJson(req): ApiJson<RealtimeTokenRequest>,
) -> Result<Json<RealtimeTokenResponse>, ApiError> {
    if let Some(l) = &req.locale
        && (l.len() > 35
            || !l
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(ApiError::InvalidRequest(
            "The locale is not a BCP 47 tag.".into(),
        ));
    }
    let provider = match req.provider {
        RealtimeProvider::Openai => "openai",
    };

    // The audit table is the limiter's source of truth, so the budget holds across restarts
    // and instances. The advisory lock makes check-then-insert atomic per account.
    let max = i64::from(state.config.limits.realtime_per_account);
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(auth.account_id.to_string())
        .execute(&mut *tx)
        .await?;
    let (count, oldest): (i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT count(*), min(created_at) FROM realtime_token_audit \
         WHERE account_id = $1 AND action = $2 AND created_at > now() - interval '1 hour'",
    )
    .bind(auth.account_id)
    .bind(AUDIT_ACTION)
    .fetch_one(&mut *tx)
    .await?;
    if count >= max {
        let until = oldest.map_or(3600, |t| {
            (t + Duration::hours(1) - Utc::now()).num_seconds()
        });
        return Err(ApiError::RateLimited {
            retry_after_secs: u64::try_from(until).unwrap_or(1).max(1),
        });
    }
    sqlx::query(
        "INSERT INTO realtime_token_audit (account_id, action, provider) VALUES ($1, $2, $3)",
    )
    .bind(auth.account_id)
    .bind(AUDIT_ACTION)
    .bind(provider)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let (token, expires_at, fake) = match &state.config.realtime {
        RealtimeConfig::Fake => {
            let t = format!(
                "fake_{}",
                crypto::random_token().map_err(ApiError::internal)?
            );
            (t, Utc::now() + Duration::seconds(TOKEN_TTL_SECS), true)
        }
        RealtimeConfig::Openai {
            api_key,
            base_url,
            transcribe_model,
        } => {
            let (t, e) = mint_openai(
                &state,
                api_key,
                base_url,
                transcribe_model,
                req.locale.as_deref(),
            )
            .await?;
            (t, e, false)
        }
    };
    Ok(Json(RealtimeTokenResponse {
        provider: req.provider,
        token,
        expires_at: rfc3339(expires_at),
        fake,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_hint_takes_two_letter_codes() {
        assert_eq!(language_hint(Some("pt-BR")).as_deref(), Some("pt"));
        assert_eq!(language_hint(Some("en")).as_deref(), Some("en"));
        assert_eq!(language_hint(Some("fil-PH")), None);
        assert_eq!(language_hint(Some("zh-CN")), None);
        assert_eq!(language_hint(None), None);
    }
}
