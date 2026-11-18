//! Provider keys, model choices and the spend cap: everything behind the Models settings page.
//! Keychain and Ollama calls block or wait on the network, so they run off the event loop and
//! report back over [`BgMsg`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sotto_protocol::ui::{
    ANTHROPIC_MODELS, ApiKey, OPENAI_MODELS, ProviderId, ProviderView, SettingsView, is_known_model,
};
use tokio::sync::mpsc;

use super::locale;
use super::models::ModelFamily;
use super::{Notice, NoticeKind, Settings};
use crate::providers::OllamaProvider;
use crate::secrets::{Account, SecretStore};
use crate::storage::Db;

/// A stopped or missing Ollama must not make the settings page feel stuck.
pub(crate) const OLLAMA_PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);

const KEY_AUTO_MODEL: &str = "auto_answer_model";
const KEY_MANUAL_MODEL: &str = "manual_model";
const KEY_SPEND_CAP: &str = "spend_cap_micro_usd";
const KEY_AUTO_ANSWER: &str = "auto_answer";
const KEY_LOCALE: &str = "locale";
/// A number of days, or `null` for "keep forever".
const KEY_RETENTION_DAYS: &str = "retention_days";
const MAX_RETENTION_DAYS: u64 = 3_650;

/// What the local model server reports. A trait so tests do not depend on a real Ollama.
#[async_trait]
pub trait LocalModels: Send + Sync {
    /// Installed model names, or `None` when the server does not answer.
    async fn ollama_models(&self) -> Option<Vec<String>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OllamaCatalog;

#[async_trait]
impl LocalModels for OllamaCatalog {
    async fn ollama_models(&self) -> Option<Vec<String>> {
        OllamaProvider::new().ok()?.list_models().await.ok()
    }
}

#[must_use]
pub(crate) fn account(provider: ProviderId) -> Option<Account> {
    match provider {
        ProviderId::Anthropic => Some(Account::ProviderAnthropic),
        ProviderId::Openai => Some(Account::ProviderOpenai),
        ProviderId::Ollama => None,
    }
}

fn provider_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Anthropic => "Anthropic",
        ProviderId::Openai => "OpenAI",
        ProviderId::Ollama => "Ollama",
    }
}

pub(crate) enum SecretJob {
    Set(ProviderId, ApiKey),
    Remove(ProviderId),
    /// Read both masks, for the first snapshot.
    Load,
}

pub(crate) enum BgMsg {
    /// The mask now stored for a provider (`None` when no key is stored).
    Mask(ProviderId, Option<String>),
    Ollama(Option<Vec<String>>),
    Notice(Notice),
    /// A key was written; clears an older notice.
    KeySaved,
    Onboarding(super::onboarding::Msg),
    Modes(super::modes::ModesMsg),
}

/// Runs Keychain jobs one at a time, in order, so a quick replace-then-remove cannot land
/// reversed.
pub(crate) fn spawn_secret_worker(
    secrets: Arc<dyn SecretStore>,
    out: mpsc::UnboundedSender<BgMsg>,
) -> mpsc::UnboundedSender<SecretJob> {
    let (tx, mut rx) = mpsc::unbounded_channel::<SecretJob>();
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            let secrets = Arc::clone(&secrets);
            let result = tokio::task::spawn_blocking(move || run_job(secrets.as_ref(), job)).await;
            match result {
                Ok(msgs) => {
                    for m in msgs {
                        let _ = out.send(m);
                    }
                }
                Err(_) => {
                    let _ = out.send(BgMsg::Notice(Notice {
                        kind: NoticeKind::Keychain,
                        message: "The Keychain task stopped unexpectedly".into(),
                    }));
                }
            }
        }
    });
    tx
}

fn run_job(secrets: &dyn SecretStore, job: SecretJob) -> Vec<BgMsg> {
    match job {
        SecretJob::Set(provider, key) => {
            let Some(acct) = account(provider) else {
                return Vec::new();
            };
            match secrets.set(acct, key.expose()) {
                Ok(()) => {
                    let mut msgs = mask_msgs(secrets, provider);
                    msgs.push(BgMsg::KeySaved);
                    msgs
                }
                Err(e) => {
                    tracing::warn!(provider = provider_name(provider), "keychain write failed");
                    // A backend error may quote the value it was given.
                    let detail = e.to_string().replace(key.expose(), "<key>");
                    vec![keychain_notice(provider, "save", &detail)]
                }
            }
        }
        SecretJob::Remove(provider) => {
            let Some(acct) = account(provider) else {
                return Vec::new();
            };
            match secrets.delete(acct) {
                Ok(()) => vec![BgMsg::Mask(provider, None)],
                Err(e) => {
                    tracing::warn!(provider = provider_name(provider), "keychain delete failed");
                    vec![keychain_notice(provider, "remove", &e.to_string())]
                }
            }
        }
        SecretJob::Load => [ProviderId::Anthropic, ProviderId::Openai]
            .into_iter()
            .flat_map(|p| mask_msgs(secrets, p))
            .collect(),
    }
}

fn mask_msgs(secrets: &dyn SecretStore, provider: ProviderId) -> Vec<BgMsg> {
    let Some(acct) = account(provider) else {
        return Vec::new();
    };
    match secrets.masked(acct) {
        Ok(mask) => vec![BgMsg::Mask(provider, mask)],
        Err(e) => vec![keychain_notice(provider, "read", &e.to_string())],
    }
}

fn keychain_notice(provider: ProviderId, action: &str, detail: &str) -> BgMsg {
    BgMsg::Notice(Notice {
        kind: NoticeKind::Keychain,
        message: format!(
            "Could not {action} the {} key in the Keychain: {detail}",
            provider_name(provider)
        ),
    })
}

/// Asks the local server for its models without blocking anything.
pub(crate) fn spawn_ollama_probe(local: Arc<dyn LocalModels>, out: mpsc::UnboundedSender<BgMsg>) {
    tokio::spawn(async move {
        let models = tokio::time::timeout(OLLAMA_PROBE_TIMEOUT, local.ollama_models())
            .await
            .ok()
            .flatten();
        let _ = out.send(BgMsg::Ollama(models));
    });
}

/// The cheap model of the same provider, for yes/no question checks. Without this an
/// OpenAI-only user would see every automatic answer fail on a missing Anthropic key.
pub(crate) fn classifier_for(auto_answer_model: &str, current: &str) -> String {
    match ModelFamily::of(auto_answer_model) {
        Some(ModelFamily::Anthropic) => "claude-haiku-5-5".into(),
        Some(ModelFamily::OpenAi) => "gpt-5-mini".into(),
        Some(ModelFamily::Ollama) => auto_answer_model.into(),
        None => current.into(),
    }
}

/// Overrides the start-up settings with what the user saved. Unreadable or unknown values are
/// ignored, so a bad row never blocks the app.
pub(crate) fn apply_saved(settings: &mut Settings, db: &Db) {
    let get = |key| db.get_setting(key).ok().flatten();
    let model = |key| {
        get(key)
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|m| is_known_model(m))
    };
    if let Some(auto) = model(KEY_AUTO_MODEL) {
        settings.classifier_model = classifier_for(&auto, &settings.classifier_model);
        settings.auto_answer_model = auto;
    }
    if let Some(manual) = model(KEY_MANUAL_MODEL) {
        settings.manual_model = manual;
    }
    if let Some(cap) = get(KEY_SPEND_CAP).and_then(|v| v.as_u64())
        && cap <= sotto_protocol::ui::MAX_SPEND_CAP_MICRO_USD
    {
        settings.guards.spend_cap_micro_usd = cap;
    }
    if let Some(enabled) = get(KEY_AUTO_ANSWER).and_then(|v| v.as_bool()) {
        settings.auto_answer = enabled;
    }
    if let Some(tag) = get(KEY_LOCALE)
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|t| sotto_protocol::ui::is_bcp47_tag(t))
    {
        settings.locale = Some(tag);
    }
    match get(KEY_RETENTION_DAYS) {
        Some(serde_json::Value::Null) => settings.retention_days = None,
        Some(v) => {
            if let Some(days) = v
                .as_u64()
                .filter(|d| (1..=MAX_RETENTION_DAYS).contains(d))
                .and_then(|d| u32::try_from(d).ok())
            {
                settings.retention_days = Some(days);
            }
        }
        None => {}
    }
}

pub(crate) fn auto_answer_row(enabled: bool) -> (&'static str, serde_json::Value) {
    (KEY_AUTO_ANSWER, enabled.into())
}

pub(crate) fn locale_row(tag: &str) -> (&'static str, serde_json::Value) {
    (KEY_LOCALE, tag.into())
}

pub(crate) fn models_rows(auto: &str, manual: &str) -> [(&'static str, serde_json::Value); 2] {
    [
        (KEY_AUTO_MODEL, auto.into()),
        (KEY_MANUAL_MODEL, manual.into()),
    ]
}

pub(crate) fn spend_cap_row(micro_usd: u64) -> (&'static str, serde_json::Value) {
    (KEY_SPEND_CAP, micro_usd.into())
}

/// What the core knows about providers between refreshes.
#[derive(Debug, Default)]
pub(crate) struct ProviderState {
    pub anthropic_mask: Option<String>,
    pub openai_mask: Option<String>,
    pub ollama_models: Option<Vec<String>>,
    /// The last list the audio helper reported in `Ready`.
    pub installed_locales: Vec<String>,
}

impl ProviderState {
    pub fn set_mask(&mut self, provider: ProviderId, mask: Option<String>) {
        match provider {
            ProviderId::Anthropic => self.anthropic_mask = mask,
            ProviderId::Openai => self.openai_mask = mask,
            ProviderId::Ollama => {}
        }
    }

    /// The locale the next session uses.
    pub fn locale(&self, settings: &Settings) -> String {
        if let Some(tag) = super::cloud_settings::cloud_locale(settings) {
            return tag;
        }
        locale::resolve(
            settings.locale.as_deref(),
            settings.system_locale.as_deref(),
            &self.installed_locales,
        )
    }

    pub fn view(&self, settings: &Settings) -> SettingsView {
        let cloud = |provider, mask: &Option<String>, models: &[&str]| ProviderView {
            provider,
            configured: mask.is_some(),
            masked_key: mask.clone(),
            models: models.iter().map(|m| (*m).to_string()).collect(),
        };
        SettingsView {
            providers: vec![
                cloud(
                    ProviderId::Anthropic,
                    &self.anthropic_mask,
                    &ANTHROPIC_MODELS,
                ),
                cloud(ProviderId::Openai, &self.openai_mask, &OPENAI_MODELS),
                ProviderView {
                    provider: ProviderId::Ollama,
                    configured: self.ollama_models.is_some(),
                    masked_key: None,
                    models: self
                        .ollama_models
                        .iter()
                        .flatten()
                        .map(|m| format!("ollama:{m}"))
                        .collect(),
                },
            ],
            auto_answer_model: settings.auto_answer_model.clone(),
            manual_model: settings.manual_model.clone(),
            spend_cap_micro_usd: settings.guards.spend_cap_micro_usd,
            ollama_running: self.ollama_models.is_some(),
            locale: self.locale(settings),
            available_locales: self.installed_locales.clone(),
            engine_preference: settings.engine_preference,
            cloud_transcription_available: self.openai_mask.is_some(),
        }
    }
}
