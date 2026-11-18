//! Where the cloud engine gets its bearer token. The BYOK source reads the user's OpenAI key from
//! the Keychain; the hosted source (ADR 0005) asks the backend for a short-lived token through
//! the account task. [`FirstAvailable`] tries them in that order.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::account::{AccountError, AccountHandle};
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

/// A short-lived OpenAI token minted by the Sotto backend for the signed-in account.
#[derive(Clone)]
pub struct HostedOpenAi {
    account: AccountHandle,
}

impl HostedOpenAi {
    #[must_use]
    pub fn new(account: AccountHandle) -> Self {
        Self { account }
    }
}

impl std::fmt::Debug for HostedOpenAi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedOpenAi").finish_non_exhaustive()
    }
}

fn expiry_instant(rfc3339: &str) -> Option<Instant> {
    let at = time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
        .ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let ms = u64::try_from(at.unix_timestamp_nanos() / 1_000_000).ok()?;
    let left = Duration::from_millis(ms).saturating_sub(now);
    Some(Instant::now() + left)
}

#[async_trait]
impl CloudCredentials for HostedOpenAi {
    async fn credential(&self) -> Result<Credential, CredentialError> {
        let minted = match self.account.realtime_token().await {
            Ok(t) => t,
            Err(AccountError::NotSignedIn) => return Err(CredentialError::Missing),
            Err(e) => return Err(CredentialError::Unavailable(e.to_string())),
        };
        // A fake token is useless upstream; refuse it so no audio goes to OpenAI with it.
        if minted.fake {
            return Err(CredentialError::Unavailable(
                "the Sotto backend is running its development transcription provider".into(),
            ));
        }
        let expires_at = expiry_instant(&minted.expires_at).ok_or_else(|| {
            CredentialError::Unavailable("Sotto sent a token expiry the app cannot read".into())
        })?;
        Ok(Credential::new(minted.token, Some(expires_at)))
    }
}

/// Tries each source in order and moves on only when one has nothing to offer. Any other
/// failure is returned, so a Keychain error never silently switches the user to hosted mode.
pub struct FirstAvailable {
    sources: Vec<Arc<dyn CloudCredentials>>,
}

impl FirstAvailable {
    #[must_use]
    pub fn new(sources: Vec<Arc<dyn CloudCredentials>>) -> Self {
        Self { sources }
    }
}

impl std::fmt::Debug for FirstAvailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirstAvailable")
            .field("sources", &self.sources)
            .finish()
    }
}

#[async_trait]
impl CloudCredentials for FirstAvailable {
    async fn credential(&self) -> Result<Credential, CredentialError> {
        for source in &self.sources {
            match source.credential().await {
                Err(CredentialError::Missing) => {}
                other => return other,
            }
        }
        Err(CredentialError::Missing)
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

    #[derive(Debug)]
    struct Fixed(Result<&'static str, CredentialError>);

    #[async_trait]
    impl CloudCredentials for Fixed {
        async fn credential(&self) -> Result<Credential, CredentialError> {
            self.0.clone().map(|t| Credential::new(t, None))
        }
    }

    fn chain(sources: [Result<&'static str, CredentialError>; 2]) -> FirstAvailable {
        FirstAvailable::new(
            sources
                .into_iter()
                .map(|s| Arc::new(Fixed(s)) as Arc<dyn CloudCredentials>)
                .collect(),
        )
    }

    #[tokio::test]
    async fn the_chain_prefers_the_first_source_and_falls_through_only_on_missing() {
        let missing = || Err(CredentialError::Missing);
        let c = chain([Ok("own"), Ok("hosted")]).credential().await.unwrap();
        assert_eq!(c.bearer(), "own");
        let c = chain([missing(), Ok("hosted")]).credential().await.unwrap();
        assert_eq!(c.bearer(), "hosted");
        assert_eq!(
            chain([missing(), missing()])
                .credential()
                .await
                .unwrap_err(),
            CredentialError::Missing
        );
        let broken = Err(CredentialError::Store("locked".into()));
        assert_eq!(
            chain([broken, Ok("hosted")])
                .credential()
                .await
                .unwrap_err(),
            CredentialError::Store("locked".into())
        );
    }

    #[test]
    fn expiry_comes_from_the_rfc3339_time() {
        let at = SystemTime::now() + Duration::from_secs(600);
        let secs = at.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let text = time::OffsetDateTime::from_unix_timestamp(i64::try_from(secs).unwrap())
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        let left = expiry_instant(&text).unwrap() - Instant::now();
        assert!(left > Duration::from_secs(590) && left <= Duration::from_secs(600));
        assert!(expiry_instant("tomorrow").is_none());
    }
}
