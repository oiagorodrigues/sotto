//! Keychain access. Secrets are read only by the core and never logged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const SERVICE: &str = "dev.sotto.app";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Account {
    ProviderAnthropic,
    ProviderOpenai,
    DbKey,
    AuthRefresh,
    CalendarClientId,
    CalendarClientSecret,
    CalendarRefresh,
}

impl Account {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ProviderAnthropic => "provider.anthropic",
            Self::ProviderOpenai => "provider.openai",
            Self::DbKey => "db.key",
            Self::AuthRefresh => "auth.refresh",
            Self::CalendarClientId => "calendar.client_id",
            Self::CalendarClientSecret => "calendar.client_secret",
            Self::CalendarRefresh => "calendar.refresh",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("secret store lock poisoned")]
    Poisoned,
}

/// Storage backend; the real one is the macOS Keychain, tests use [`MemoryStore`].
pub trait SecretStore: Send + Sync {
    fn get(&self, account: Account) -> Result<Option<String>, SecretsError>;
    fn set(&self, account: Account, value: &str) -> Result<(), SecretsError>;
    /// Deleting a missing secret is not an error.
    fn delete(&self, account: Account) -> Result<(), SecretsError>;

    /// `sk-ant-…4f2a` style: first 6 and last 4 characters, nothing else.
    fn masked(&self, account: Account) -> Result<Option<String>, SecretsError> {
        Ok(self.get(account)?.map(|v| mask(&v)))
    }
}

#[must_use]
pub fn mask(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 16 {
        return "…".to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainStore;

impl KeychainStore {
    fn entry(account: Account) -> Result<keyring::Entry, SecretsError> {
        keyring::Entry::new(SERVICE, account.name())
            .map_err(|e| SecretsError::Keychain(e.to_string()))
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, account: Account) -> Result<Option<String>, SecretsError> {
        match Self::entry(account)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretsError::Keychain(e.to_string())),
        }
    }

    fn set(&self, account: Account, value: &str) -> Result<(), SecretsError> {
        Self::entry(account)?
            .set_password(value)
            .map_err(|e| SecretsError::Keychain(e.to_string()))
    }

    fn delete(&self, account: Account) -> Result<(), SecretsError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretsError::Keychain(e.to_string())),
        }
    }
}

#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<Account, String>>);

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, account: Account) -> Result<Option<String>, SecretsError> {
        let map = self.0.lock().map_err(|_| SecretsError::Poisoned)?;
        Ok(map.get(&account).cloned())
    }

    fn set(&self, account: Account, value: &str) -> Result<(), SecretsError> {
        let mut map = self.0.lock().map_err(|_| SecretsError::Poisoned)?;
        map.insert(account, value.to_string());
        Ok(())
    }

    fn delete(&self, account: Account) -> Result<(), SecretsError> {
        let mut map = self.0.lock().map_err(|_| SecretsError::Poisoned)?;
        map.remove(&account);
        Ok(())
    }
}

/// Reads each secret from the backing store once and keeps it in memory, so the Keychain is
/// not asked (and an ad-hoc signed dev build does not prompt) on every answer. Writes and
/// deletes go through to the store and update the cache; a failed one drops the entry so the
/// next read asks the store again. A change made outside the app is seen after a restart.
pub struct CachedStore {
    inner: Arc<dyn SecretStore>,
    cache: Mutex<HashMap<Account, Option<String>>>,
}

impl std::fmt::Debug for CachedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedStore").finish_non_exhaustive()
    }
}

impl CachedStore {
    #[must_use]
    pub fn new(inner: Arc<dyn SecretStore>) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl SecretStore for CachedStore {
    fn get(&self, account: Account) -> Result<Option<String>, SecretsError> {
        // Held across the read so two first reads cannot both reach the Keychain.
        let mut cache = self.cache.lock().map_err(|_| SecretsError::Poisoned)?;
        if let Some(v) = cache.get(&account) {
            return Ok(v.clone());
        }
        let v = self.inner.get(account)?;
        cache.insert(account, v.clone());
        Ok(v)
    }

    fn set(&self, account: Account, value: &str) -> Result<(), SecretsError> {
        let mut cache = self.cache.lock().map_err(|_| SecretsError::Poisoned)?;
        let result = self.inner.set(account, value);
        match &result {
            Ok(()) => cache.insert(account, Some(value.to_string())),
            Err(_) => cache.remove(&account),
        };
        result
    }

    fn delete(&self, account: Account) -> Result<(), SecretsError> {
        let mut cache = self.cache.lock().map_err(|_| SecretsError::Poisoned)?;
        let result = self.inner.delete(account);
        match &result {
            Ok(()) => cache.insert(account, None),
            Err(_) => cache.remove(&account),
        };
        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Counting {
        store: MemoryStore,
        reads: AtomicUsize,
    }

    impl SecretStore for Counting {
        fn get(&self, account: Account) -> Result<Option<String>, SecretsError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.store.get(account)
        }
        fn set(&self, account: Account, value: &str) -> Result<(), SecretsError> {
            self.store.set(account, value)
        }
        fn delete(&self, account: Account) -> Result<(), SecretsError> {
            self.store.delete(account)
        }
    }

    #[test]
    fn cached_store_reads_once_and_follows_writes() {
        let inner = Arc::new(Counting::default());
        inner.store.set(Account::ProviderOpenai, "sk-one").unwrap();
        let c = CachedStore::new(Arc::clone(&inner) as Arc<dyn SecretStore>);
        assert_eq!(
            c.get(Account::ProviderOpenai).unwrap().as_deref(),
            Some("sk-one")
        );
        assert_eq!(
            c.get(Account::ProviderOpenai).unwrap().as_deref(),
            Some("sk-one")
        );
        assert_eq!(c.get(Account::ProviderAnthropic).unwrap(), None);
        assert_eq!(c.get(Account::ProviderAnthropic).unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::SeqCst), 2);
        c.set(Account::ProviderOpenai, "sk-two").unwrap();
        assert_eq!(
            c.get(Account::ProviderOpenai).unwrap().as_deref(),
            Some("sk-two")
        );
        c.delete(Account::ProviderOpenai).unwrap();
        assert_eq!(c.get(Account::ProviderOpenai).unwrap(), None);
        assert_eq!(inner.store.get(Account::ProviderOpenai).unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn account_names_are_stable() {
        assert_eq!(Account::ProviderAnthropic.name(), "provider.anthropic");
        assert_eq!(Account::DbKey.name(), "db.key");
        assert_eq!(SERVICE, "dev.sotto.app");
    }

    #[test]
    fn set_get_delete_round_trip() {
        let s = MemoryStore::new();
        assert_eq!(s.get(Account::ProviderOpenai).unwrap(), None);
        s.set(Account::ProviderOpenai, "sk-abc").unwrap();
        assert_eq!(
            s.get(Account::ProviderOpenai).unwrap().as_deref(),
            Some("sk-abc")
        );
        s.delete(Account::ProviderOpenai).unwrap();
        s.delete(Account::ProviderOpenai).unwrap();
        assert_eq!(s.get(Account::ProviderOpenai).unwrap(), None);
    }

    #[test]
    fn masks_keep_only_edges() {
        let s = MemoryStore::new();
        s.set(Account::ProviderAnthropic, "sk-ant-api03-SECRETSECRET-4f2a")
            .unwrap();
        assert_eq!(
            s.masked(Account::ProviderAnthropic).unwrap().as_deref(),
            Some("sk-ant…4f2a")
        );
        assert_eq!(s.masked(Account::AuthRefresh).unwrap(), None);
        assert_eq!(mask("short"), "…");
    }
}
