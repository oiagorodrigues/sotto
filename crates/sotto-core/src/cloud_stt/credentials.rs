//! Where the cloud engine gets its bearer token. The BYOK implementation reads the user's
//! OpenAI key from the Keychain; hosted mode (ADR 0005) will mint a short-lived token through
//! the backend and return it with an expiry.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use crate::secrets::{Account, SecretStore};

/// A bearer token. `Debug` never prints it.
#[derive(Clone)]
pub struct Credential {
    token: String,
    /// `None` for a key that does not expire. A token is only checked when a connection is
    /// opened, so an open socket outlives it.
    expires_at: Option<Instant>,
}

impl Credential {
    #[must_use]
    pub fn new(token: impl Into<String>, expires_at: Option<Instant>) -> Self {
        Self {
            token: token.into(),
            expires_at,
        }
    }

    #[must_use]
    pub fn bearer(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub fn expires_at(&self) -> Option<Instant> {
        self.expires_at
    }

    #[must_use]
    pub fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("no OpenAI key is saved")]
    Missing,
    #[error("cannot read the OpenAI key: {0}")]
    Store(String),
    #[error("cannot get a cloud transcription token: {0}")]
    Unavailable(String),
}

/// Called each time a connection is opened, so a reconnect gets a fresh token.
#[async_trait]
pub trait CloudCredentials: Send + Sync + std::fmt::Debug {
    async fn credential(&self) -> Result<Credential, CredentialError>;
}

/// The user's own OpenAI key. The Keychain read blocks, so it runs off the async workers.
#[derive(Clone)]
pub struct ByokOpenAi {
    secrets: Arc<dyn SecretStore>,
}

impl ByokOpenAi {
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self { secrets }
    }
}

impl std::fmt::Debug for ByokOpenAi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ByokOpenAi").finish_non_exhaustive()
    }
}

#[async_trait]
impl CloudCredentials for ByokOpenAi {
    async fn credential(&self) -> Result<Credential, CredentialError> {
        let secrets = Arc::clone(&self.secrets);
        let key = tokio::task::spawn_blocking(move || secrets.get(Account::ProviderOpenai))
            .await
            .map_err(|_| CredentialError::Store("the Keychain task stopped".into()))?
            .map_err(|e| CredentialError::Store(e.to_string()))?;
        key.filter(|k| !k.is_empty())
            .map(|k| Credential::new(k, None))
            .ok_or(CredentialError::Missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    #[tokio::test]
    async fn byok_reads_the_openai_key_and_reports_a_missing_one() {
        let store = Arc::new(MemoryStore::new());
        let creds = ByokOpenAi::new(store.clone());
        assert_eq!(
            creds.credential().await.unwrap_err(),
            CredentialError::Missing
        );
        store
            .set(Account::ProviderOpenai, "sk-test-secret-value")
            .unwrap();
        let c = creds.credential().await.unwrap();
        assert_eq!(c.bearer(), "sk-test-secret-value");
        assert_eq!(c.expires_at(), None);
        assert!(!c.is_expired(Instant::now()));
    }

    #[test]
    fn debug_output_hides_the_token() {
        let c = Credential::new("sk-test-secret-value", None);
        assert!(!format!("{c:?}").contains("secret"));
    }

    #[test]
    fn expiry_is_checked_against_the_given_time() {
        let now = Instant::now();
        let c = Credential::new("t", Some(now));
        assert!(c.is_expired(now));
        assert!(!c.is_expired(now - std::time::Duration::from_secs(1)));
    }
}
