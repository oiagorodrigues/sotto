use std::collections::HashSet;

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use sotto_protocol::api::{
    MAX_MODES, MAX_PROMPT_CHARS, Preferences, PreferencesDocument, PutPreferences,
};
use uuid::Uuid;

use crate::{ApiJson, AppState, Authed, error::ApiError, rfc3339};

fn invalid(msg: &str) -> ApiError {
    ApiError::InvalidRequest(msg.to_owned())
}

fn plain_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

fn validate(p: &Preferences) -> Result<(), ApiError> {
    let s = &p.settings;
    if !plain_text(&s.auto_answer_model, 100) || !plain_text(&s.manual_model, 100) {
        return Err(invalid("Model ids must be 1 to 100 characters."));
    }
    if s.spend_cap_micro_usd > 1_000_000_000 {
        return Err(invalid("The spend cap is too large."));
    }
    let locale_ok = (2..=35).contains(&s.locale.len())
        && s.locale
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !locale_ok {
        return Err(invalid("The locale is not a BCP 47 tag."));
    }
    if !matches!(s.engine_preference.as_str(), "local" | "cloud" | "auto") {
        return Err(invalid("engine_preference must be local, cloud or auto."));
    }
    if p.shortcuts.len() > 50 {
        return Err(invalid("Too many shortcuts."));
    }
    for (action, accelerator) in &p.shortcuts {
        if !plain_text(action, 64) || !plain_text(accelerator, 64) {
            return Err(invalid(
                "Shortcut names and keys must be 1 to 64 characters.",
            ));
        }
    }
    if p.modes.len() > MAX_MODES {
        return Err(invalid("Too many modes."));
    }
    let mut ids = HashSet::new();
    for m in &p.modes {
        let id_ok = (1..=64).contains(&m.id.len())
            && m.id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !id_ok || !ids.insert(m.id.as_str()) {
            return Err(invalid(
                "Mode ids must be unique, 1 to 64 characters of letters, digits, - and _.",
            ));
        }
        if !plain_text(&m.name, 100) {
            return Err(invalid("Mode names must be 1 to 100 characters."));
        }
        if m.system_prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err(invalid("A mode prompt is too long."));
        }
        if m.updated_at.len() > 40 || DateTime::parse_from_rfc3339(&m.updated_at).is_err() {
            return Err(invalid("Mode updated_at must be an RFC 3339 time."));
        }
    }
    Ok(())
}

async fn load(db: &sqlx::PgPool, account_id: Uuid) -> Result<PreferencesDocument, ApiError> {
    let row: Option<(i64, serde_json::Value, DateTime<Utc>)> = sqlx::query_as(
        "SELECT version, document, updated_at FROM preferences WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_optional(db)
    .await?;
    match row {
        None => Ok(PreferencesDocument {
            version: 0,
            preferences: None,
            updated_at: None,
        }),
        Some((version, doc, updated_at)) => Ok(PreferencesDocument {
            version: u64::try_from(version).map_err(ApiError::internal)?,
            preferences: Some(serde_json::from_value(doc).map_err(ApiError::internal)?),
            updated_at: Some(rfc3339(updated_at)),
        }),
    }
}

pub async fn get_preferences(
    State(state): State<AppState>,
    auth: Authed,
) -> Result<Json<PreferencesDocument>, ApiError> {
    Ok(Json(load(&state.db, auth.account_id).await?))
}

pub async fn put_preferences(
    State(state): State<AppState>,
    auth: Authed,
    ApiJson(req): ApiJson<PutPreferences>,
) -> Result<Json<PreferencesDocument>, ApiError> {
    validate(&req.preferences)?;
    let base =
        i64::try_from(req.base_version).map_err(|_| invalid("base_version is too large."))?;
    let doc = serde_json::to_value(&req.preferences).map_err(ApiError::internal)?;
    let written: Option<(DateTime<Utc>,)> = if base == 0 {
        sqlx::query_as(
            "INSERT INTO preferences (account_id, version, document) VALUES ($1, 1, $2) \
             ON CONFLICT (account_id) DO NOTHING RETURNING updated_at",
        )
        .bind(auth.account_id)
        .bind(&doc)
        .fetch_optional(&state.db)
        .await?
    } else {
        sqlx::query_as(
            "UPDATE preferences SET version = version + 1, document = $2, updated_at = now() \
             WHERE account_id = $1 AND version = $3 RETURNING updated_at",
        )
        .bind(auth.account_id)
        .bind(&doc)
        .bind(base)
        .fetch_optional(&state.db)
        .await?
    };
    match written {
        Some((updated_at,)) => Ok(Json(PreferencesDocument {
            version: req.base_version + 1,
            preferences: Some(req.preferences),
            updated_at: Some(rfc3339(updated_at)),
        })),
        None => Err(ApiError::Conflict(Box::new(
            load(&state.db, auth.account_id).await?,
        ))),
    }
}
