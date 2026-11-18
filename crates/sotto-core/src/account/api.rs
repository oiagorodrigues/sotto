//! HTTP calls to the Sotto backend, typed by `sotto_protocol::api`.

use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use sotto_protocol::api::{
    ErrorBody, MeResponse, PreferencesConflict, PreferencesDocument, PutPreferences, TokenRequest,
    TokenResponse,
};

use super::error::AccountError;
use super::url::BackendUrl;

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub(super) struct Client {
    http: reqwest::Client,
    backend: BackendUrl,
}

impl Client {
    pub fn new(backend: BackendUrl) -> Result<Self, AccountError> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| AccountError::from_http(&e))?;
        Ok(Self { http, backend })
    }

    pub fn backend(&self) -> &BackendUrl {
        &self.backend
    }

    pub async fn token(&self, request: &TokenRequest) -> Result<TokenResponse, AccountError> {
        let req = self
            .http
            .post(self.backend.endpoint("/v1/oauth/token"))
            .json(request);
        parse(send(req).await?)
    }

    pub async fn me(&self, access: &str) -> Result<MeResponse, AccountError> {
        let req = self
            .http
            .get(self.backend.endpoint("/v1/me"))
            .bearer_auth(access);
        parse(send(req).await?)
    }

    pub async fn logout(&self, access: &str) -> Result<(), AccountError> {
        let req = self
            .http
            .post(self.backend.endpoint("/v1/logout"))
            .bearer_auth(access);
        send(req).await?;
        Ok(())
    }

    pub async fn get_preferences(&self, access: &str) -> Result<PreferencesDocument, AccountError> {
        let req = self
            .http
            .get(self.backend.endpoint("/v1/preferences"))
            .bearer_auth(access);
        parse(send(req).await?)
    }

    pub async fn put_preferences(
        &self,
        access: &str,
        body: &PutPreferences,
    ) -> Result<PreferencesDocument, AccountError> {
        let req = self
            .http
            .put(self.backend.endpoint("/v1/preferences"))
            .bearer_auth(access)
            .json(body);
        parse(send(req).await?)
    }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

async fn send(req: reqwest::RequestBuilder) -> Result<Reply, AccountError> {
    let response = req.send().await.map_err(|e| AccountError::from_http(&e))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|e| AccountError::from_http(&e))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(AccountError::BadResponse);
    }
    let reply = Reply {
        status,
        body: body.to_vec(),
    };
    if status.is_success() {
        Ok(reply)
    } else {
        Err(error_for(&reply))
    }
}

fn parse<T: DeserializeOwned>(reply: Reply) -> Result<T, AccountError> {
    serde_json::from_slice(&reply.body).map_err(|_| AccountError::BadResponse)
}

fn error_for(reply: &Reply) -> AccountError {
    if reply.status == StatusCode::CONFLICT
        && let Ok(conflict) = serde_json::from_slice::<PreferencesConflict>(&reply.body)
    {
        return AccountError::Conflict(Box::new(conflict.current));
    }
    let code = serde_json::from_slice::<ErrorBody>(&reply.body)
        .ok()
        .map(|b| b.error)
        .filter(|c| c.len() <= 32 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
    match (reply.status, code.as_deref()) {
        (StatusCode::UNAUTHORIZED, _) => AccountError::Unauthorized,
        (_, Some("invalid_grant")) => AccountError::InvalidGrant,
        (StatusCode::TOO_MANY_REQUESTS, _) => AccountError::RateLimited,
        (status, code) => {
            AccountError::Rejected(code.map_or_else(|| status.as_u16().to_string(), str::to_string))
        }
    }
}
