//! Model ids to providers. Ids are `claude-*` (Anthropic), `gpt-*` or `o<digit>*` (OpenAI) and
//! `ollama:<name>` (a local Ollama model).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::pricing::OLLAMA_PRICE_KEY;
use crate::providers::{
    AnthropicProvider, LlmProvider, OllamaProvider, OpenAiProvider, ProviderError,
};
use crate::secrets::{Account, SecretStore};

const OLLAMA_PREFIX: &str = "ollama:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    Anthropic,
    OpenAi,
    Ollama,
}

impl ModelFamily {
    #[must_use]
    pub fn of(model: &str) -> Option<Self> {
        if model.starts_with("claude-") {
            return Some(Self::Anthropic);
        }
        if model.starts_with(OLLAMA_PREFIX) && model.len() > OLLAMA_PREFIX.len() {
            return Some(Self::Ollama);
        }
        let mut chars = model.chars();
        let o_series =
            chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit());
        if model.starts_with("gpt-") || o_series {
            return Some(Self::OpenAi);
        }
        None
    }
}

/// The model name the provider's API expects.
#[must_use]
pub fn wire_model(model: &str) -> &str {
    model.strip_prefix(OLLAMA_PREFIX).unwrap_or(model)
}

/// The key into [`crate::pricing::PriceTable`].
#[must_use]
pub fn price_key(model: &str) -> &str {
    if ModelFamily::of(model) == Some(ModelFamily::Ollama) {
        OLLAMA_PRICE_KEY
    } else {
        model
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ProviderSetupError {
    #[error("Add an {0} key in Settings > Models")]
    MissingKey(&'static str),
    #[error("Unknown model \"{0}\". Choose another one in Settings > Models")]
    UnknownModel(String),
    #[error("Cannot read the API key from the Keychain: {0}")]
    Secrets(String),
    #[error("Cannot set up the provider: {0}")]
    Provider(#[from] ProviderError),
}

/// Resolves a model id to a provider. Called from stream tasks, never from the event loop,
/// because the real one reads the Keychain.
pub trait ProviderFactory: Send + Sync {
    fn provider(
        &self,
        model: &str,
        secrets: &dyn SecretStore,
    ) -> Result<Arc<dyn LlmProvider>, ProviderSetupError>;
}

/// The key the provider was built with, and the provider.
type CachedProvider = (String, Arc<dyn LlmProvider>);

/// Builds the real providers with keys from the secret store. A provider is reused while its
/// key is unchanged so its HTTP connection pool (and TLS session) survives between answers,
/// which keeps a handshake off the answer latency.
#[derive(Default)]
pub struct DefaultProviderFactory {
    cache: Mutex<HashMap<ModelFamily, CachedProvider>>,
}

impl std::fmt::Debug for DefaultProviderFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultProviderFactory")
            .finish_non_exhaustive()
    }
}

impl DefaultProviderFactory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn cached(
        &self,
        family: ModelFamily,
        key: &str,
        build: impl FnOnce() -> Result<Arc<dyn LlmProvider>, ProviderError>,
    ) -> Result<Arc<dyn LlmProvider>, ProviderSetupError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| ProviderSetupError::Secrets("provider cache lock poisoned".into()))?;
        if let Some((cached_key, provider)) = cache.get(&family)
            && cached_key == key
        {
            return Ok(Arc::clone(provider));
        }
        let provider = build()?;
        cache.insert(family, (key.to_string(), Arc::clone(&provider)));
        Ok(provider)
    }
}

fn read_key(
    secrets: &dyn SecretStore,
    account: Account,
    provider_name: &'static str,
) -> Result<String, ProviderSetupError> {
    secrets
        .get(account)
        .map_err(|e| ProviderSetupError::Secrets(e.to_string()))?
        .filter(|k| !k.trim().is_empty())
        .ok_or(ProviderSetupError::MissingKey(provider_name))
}

impl ProviderFactory for DefaultProviderFactory {
    fn provider(
        &self,
        model: &str,
        secrets: &dyn SecretStore,
    ) -> Result<Arc<dyn LlmProvider>, ProviderSetupError> {
        let family =
            ModelFamily::of(model).ok_or_else(|| ProviderSetupError::UnknownModel(model.into()))?;
        match family {
            ModelFamily::Anthropic => {
                let key = read_key(secrets, Account::ProviderAnthropic, "Anthropic")?;
                self.cached(family, &key, || {
                    Ok(Arc::new(AnthropicProvider::new(key.clone())?) as Arc<dyn LlmProvider>)
                })
            }
            ModelFamily::OpenAi => {
                let key = read_key(secrets, Account::ProviderOpenai, "OpenAI")?;
                self.cached(family, &key, || {
                    Ok(Arc::new(OpenAiProvider::new(key.clone())?) as Arc<dyn LlmProvider>)
                })
            }
            ModelFamily::Ollama => self.cached(family, "", || {
                Ok(Arc::new(OllamaProvider::new()?) as Arc<dyn LlmProvider>)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    #[test]
    fn families_and_wire_names() {
        assert_eq!(
            ModelFamily::of("claude-haiku-5-5"),
            Some(ModelFamily::Anthropic)
        );
        assert_eq!(ModelFamily::of("gpt-4o"), Some(ModelFamily::OpenAi));
        assert_eq!(ModelFamily::of("o3-mini"), Some(ModelFamily::OpenAi));
        assert_eq!(ModelFamily::of("ollama:qwen3"), Some(ModelFamily::Ollama));
        assert_eq!(ModelFamily::of("ollama:"), None);
        assert_eq!(ModelFamily::of("omega"), None);
        assert_eq!(wire_model("ollama:qwen3:4b"), "qwen3:4b");
        assert_eq!(wire_model("claude-haiku-5-5"), "claude-haiku-5-5");
        assert_eq!(price_key("ollama:qwen3"), OLLAMA_PRICE_KEY);
        assert_eq!(price_key("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn missing_key_has_a_clear_message() {
        let f = DefaultProviderFactory::new();
        let store = MemoryStore::new();
        let err = f.provider("claude-haiku-5-5", &store).err();
        assert_eq!(err, Some(ProviderSetupError::MissingKey("Anthropic")));
        assert_eq!(
            err.map(|e| e.to_string()).as_deref(),
            Some("Add an Anthropic key in Settings > Models")
        );
        store.set(Account::ProviderOpenai, "  ").unwrap();
        assert_eq!(
            f.provider("gpt-4o", &store).err(),
            Some(ProviderSetupError::MissingKey("OpenAI"))
        );
        assert!(matches!(
            f.provider("mystery", &store),
            Err(ProviderSetupError::UnknownModel(_))
        ));
    }

    #[test]
    fn providers_are_reused_until_the_key_changes() {
        let f = DefaultProviderFactory::new();
        let store = MemoryStore::new();
        store.set(Account::ProviderAnthropic, "sk-ant-one").unwrap();
        let a = f.provider("claude-haiku-5-5", &store).unwrap();
        let b = f.provider("claude-sonnet-5-5", &store).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        store.set(Account::ProviderAnthropic, "sk-ant-two").unwrap();
        let c = f.provider("claude-haiku-5-5", &store).unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
    }
}
