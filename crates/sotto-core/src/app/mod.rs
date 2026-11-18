//! The application core: one tokio task owns all state (sessions, transcript, answers) and
//! talks to the shell through intents in and snapshots out (ADR 0006 for the answer flow).

mod calendar;
mod check;
mod cloud_settings;
mod event_loop;
mod locale;
mod mode_context;
mod mode_files;
mod mode_index;
mod models;
mod modes;
mod onboarding;
mod settings;
mod store;
mod streams;
#[cfg(test)]
mod tests;
mod wer;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sotto_detect::{DetectorConfig, GuardConfig};
use sotto_protocol::sidecar::SidecarCommand;
use sotto_protocol::ui::{AppSnapshot, EnginePreference, Intent, IntentError};
use sotto_rag::{EmbedError, Embedder, OllamaEmbedder};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

pub use self::locale::detect_system_locale;
pub use self::models::{
    DefaultProviderFactory, ModelFamily, ProviderFactory, ProviderSetupError, price_key, wire_model,
};
pub use self::settings::{LocalModels, OllamaCatalog};
use crate::account::{self, AccountConfig, AccountHandle};
use crate::calendar::{self as calendar_service, CalendarConfig};
use crate::cloud_stt::{ByokOpenAi, CloudCredentials, FirstAvailable, HostedOpenAi, PcmFrame};
use crate::pricing::{self as pricing_table, PriceTable};
use crate::providers::Effort;
use crate::secrets::{CachedStore, SecretStore};
use crate::sidecar::{SidecarError, SidecarOutput, SidecarProcess};
use crate::storage::Db;

const INTENT_QUEUE: usize = 256;
const NOTICE_QUEUE: usize = 64;
const SIDECAR_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// A running sidecar as the core sees it: a command sender and the output stream. Dropping
/// the sender shuts the process down.
#[derive(Debug)]
pub struct SidecarLink {
    pub commands: mpsc::UnboundedSender<SidecarCommand>,
    pub outputs: mpsc::Receiver<SidecarOutput>,
    /// Raw PCM frames from file descriptor 3, for the cloud engine. `None` when the helper has
    /// no such pipe.
    pub pcm: Option<mpsc::Receiver<PcmFrame>>,
}

/// Starts the sidecar. Called on the event loop, inside the tokio runtime.
pub trait SidecarFactory: Send + Sync {
    fn spawn(&self) -> Result<SidecarLink, SidecarError>;

    /// Starts a separate sidecar that transcribes `wav` at real-time pace and exits, for the
    /// onboarding check. Factories that cannot do it keep this default.
    fn spawn_file_mode(&self, wav: &Path, locale: &str) -> Result<SidecarLink, SidecarError> {
        let _ = (wav, locale);
        Err(SidecarError::Spawn(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "file mode is not available",
        )))
    }
}

/// Spawns `sotto-audio` from an absolute path (never through `PATH`).
#[derive(Debug, Clone)]
pub struct ProcessSidecarFactory {
    pub path: PathBuf,
    pub args: Vec<String>,
}

impl ProcessSidecarFactory {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            args: Vec::new(),
        }
    }
}

impl SidecarFactory for ProcessSidecarFactory {
    fn spawn(&self) -> Result<SidecarLink, SidecarError> {
        let (mut process, outputs, pcm) = SidecarProcess::spawn_with_pcm(&self.path, &self.args)?;
        let (commands, mut rx) = mpsc::unbounded_channel::<SidecarCommand>();
        // Writes can wait on a full pipe, so they happen here and never on the event loop.
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if let Err(e) = process.send(&cmd).await {
                    tracing::debug!(error = %e, "sidecar command not delivered");
                }
            }
            process.shutdown(SIDECAR_SHUTDOWN_GRACE).await;
        });
        Ok(SidecarLink {
            commands,
            outputs,
            pcm: Some(pcm),
        })
    }

    fn spawn_file_mode(&self, wav: &Path, locale: &str) -> Result<SidecarLink, SidecarError> {
        check::file_mode_link(&self.path, wav, locale)
    }
}

/// Start-up defaults. Values saved in the database override them at start, and the settings
/// intents change them while the core runs.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The user's transcription locale (BCP 47, for example `pt-BR`). `None` picks the system
    /// locale when the engine has it installed, else `en-US`.
    pub locale: Option<String>,
    /// `None` is detected at `App::start` (`detect_system_locale`).
    pub system_locale: Option<String>,
    /// Sessions older than this are deleted at start and once a day; `None` keeps them.
    pub retention_days: Option<u32>,
    pub auto_answer: bool,
    pub invisible: bool,
    pub auto_answer_model: String,
    pub classifier_model: String,
    pub manual_model: String,
    pub manual_effort: Option<Effort>,
    pub detector: DetectorConfig,
    pub guards: GuardConfig,
    /// Transcript lines kept in the snapshot (the newest ones).
    pub transcript_lines: usize,
    /// Start in `Phase::Onboarding` until the user completes it once. Off by default so a
    /// core built without a window (tests) starts idle.
    pub require_onboarding: bool,
    /// Directory with the bundled sample clips for the transcription check.
    pub clips_dir: Option<PathBuf>,
    /// OS version and build; `None` is detected at `App::start`.
    pub macos_version: Option<String>,
    /// The engine for new sessions. `Cloud` is only ever set by the user (ADR 0003).
    pub engine_preference: EnginePreference,
    /// Replaces the OpenAI Realtime endpoint; tests point it at a local server.
    pub cloud_stt_url: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            locale: None,
            system_locale: None,
            retention_days: Some(30),
            auto_answer: true,
            invisible: true,
            auto_answer_model: "claude-haiku-5-5".into(),
            classifier_model: "claude-haiku-5-5".into(),
            manual_model: "claude-sonnet-5-5".into(),
            manual_effort: Some(Effort::Medium),
            detector: DetectorConfig::default(),
            guards: GuardConfig::default(),
            transcript_lines: 200,
            require_onboarding: false,
            clips_dir: None,
            macos_version: None,
            engine_preference: EnginePreference::Local,
            cloud_stt_url: None,
        }
    }
}

pub struct AppConfig {
    pub sidecars: Arc<dyn SidecarFactory>,
    pub secrets: Arc<dyn SecretStore>,
    pub db: Db,
    /// The bundled table; `<data_dir>/pricing.toml` entries replace its entries at start.
    pub prices: PriceTable,
    pub providers: Arc<dyn ProviderFactory>,
    pub local_models: Arc<dyn LocalModels>,
    pub settings: Settings,
    /// The app data directory, where an optional `pricing.toml` override lives.
    pub data_dir: Option<PathBuf>,
    pub calendar: CalendarConfig,
    /// Embeds mode files for retrieval. `None` uses the pinned model on the local Ollama.
    pub embedder: Option<Arc<dyn Embedder>>,
    pub account: AccountConfig,
}

impl AppConfig {
    /// Real Ollama probing, default settings and no data directory; set the public fields to
    /// change them.
    #[must_use]
    pub fn new(
        sidecars: Arc<dyn SidecarFactory>,
        secrets: Arc<dyn SecretStore>,
        db: Db,
        prices: PriceTable,
        providers: Arc<dyn ProviderFactory>,
    ) -> Self {
        Self {
            sidecars,
            secrets,
            db,
            prices,
            providers,
            local_models: Arc::new(OllamaCatalog),
            settings: Settings::default(),
            data_dir: None,
            calendar: CalendarConfig::disabled(),
            embedder: None,
            account: AccountConfig::disabled(),
        }
    }
}

impl std::fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConfig")
            .field("prices_as_of", &self.prices.as_of)
            .field("settings", &self.settings)
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

/// Something the user should know that is not part of an answer. Messages never contain
/// transcript text, prompts or answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// The sidecar could not start or exited; the engine shows `Unavailable`.
    SidecarUnavailable,
    SidecarError,
    Classifier,
    Storage,
    Keychain,
    /// A model has no price, or the price override file is invalid.
    Pricing,
    /// The chosen transcription locale is not installed.
    Locale,
    /// A step of the first-run flow cannot proceed.
    Onboarding,
    /// A mode file could not be added or indexed, or retrieval fell back to plain text.
    Modes,
    /// Cloud transcription could not start or lost its connection.
    Cloud,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DispatchError {
    #[error("invalid intent: {0:?}")]
    Invalid(IntentError),
    #[error("the core is busy; try again")]
    Busy,
    #[error("the core has stopped")]
    Stopped,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("App::start must run inside a tokio runtime")]
    NoRuntime,
    #[error("cannot start the storage thread: {0}")]
    Storage(#[from] std::io::Error),
    #[error("embedding configuration: {0}")]
    Embedding(#[from] EmbedError),
}

#[derive(Debug)]
enum Control {
    Intent(Intent),
    Shutdown(oneshot::Sender<()>),
}

#[derive(Debug)]
pub struct App;

impl App {
    /// Spawns the event loop on the current tokio runtime.
    pub fn start(config: AppConfig) -> Result<AppHandle, StartError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(StartError::NoRuntime);
        }
        let (notices, _) = broadcast::channel(NOTICE_QUEUE);
        let mut settings = config.settings;
        settings::apply_saved(&mut settings, &config.db);
        cloud_settings::apply_saved(&mut settings, &config.db);
        if settings.system_locale.is_none() {
            settings.system_locale = detect_system_locale();
        }
        if settings.macos_version.is_none() {
            settings.macos_version = onboarding::detect_macos_version();
        }
        let saved_onboarding = onboarding::load(&config.db);
        let mut prices = config.prices;
        let mut start_notice = None;
        if let Some(dir) = &config.data_dir {
            let path = dir.join(pricing_table::OVERRIDE_FILE);
            match PriceTable::load_override(&path) {
                Ok(Some(more)) => prices.merge(more),
                Ok(None) => {}
                Err(e) => {
                    start_notice = Some(format!(
                        "Ignored {}: {e}. Every model needs input, output, cache_read and cache_write",
                        path.display()
                    ));
                }
            }
        }
        let embedder: Arc<dyn Embedder> = match config.embedder {
            Some(e) => e,
            None => Arc::new(OllamaEmbedder::bundled()?),
        };
        let saved_modes = modes::SavedModes::read(&config.db);
        let store = store::StoreHandle::spawn(config.db, notices.clone())?;
        let (intents_tx, intents_rx) = mpsc::channel(INTENT_QUEUE);
        let (stream_tx, stream_rx) = mpsc::unbounded_channel();
        let (bg_tx, bg_rx) = mpsc::unbounded_channel();
        let secrets: Arc<dyn SecretStore> = Arc::new(CachedStore::new(config.secrets));
        let calendar = calendar_service::spawn(config.calendar, Arc::clone(&secrets));
        let (account, account_applies) = account::spawn(config.account, Arc::clone(&secrets));
        let cloud_credentials: Arc<dyn CloudCredentials> = Arc::new(FirstAvailable::new(vec![
            Arc::new(ByokOpenAi::new(Arc::clone(&secrets))),
            Arc::new(HostedOpenAi::new(account.clone())),
        ]));
        let secret_jobs = settings::spawn_secret_worker(Arc::clone(&secrets), bg_tx.clone());
        let onboarding = onboarding::Onboarding::new(
            onboarding::Parts {
                sidecars: Arc::clone(&config.sidecars),
                providers: Arc::clone(&config.providers),
                secrets: Arc::clone(&secrets),
                bg: bg_tx.clone(),
                clips_dir: settings.clips_dir.clone(),
                macos_version: settings.macos_version.clone(),
            },
            &saved_onboarding,
        );
        let spawner = streams::Spawner::new(config.providers, secrets, stream_tx.clone());
        let modes_unreadable = saved_modes.is_none();
        let modes = modes::ModesState::new(
            store.access(),
            embedder,
            notices.clone(),
            bg_tx.clone(),
            saved_modes,
        );
        let mut initial = event_loop::initial_snapshot(&settings);
        initial.notice = start_notice;
        initial.phase = onboarding::initial_phase(settings.require_onboarding, &saved_onboarding);
        initial.onboarding = onboarding.view().clone();
        if modes_unreadable && initial.notice.is_none() {
            initial.notice = Some("Could not read the modes from the local database".into());
        }
        (initial.modes, initial.selected_mode_id) = modes.views();
        let (snap_tx, snap_rx) = watch::channel(initial.clone());
        let core = event_loop::Core::new(event_loop::CoreParts {
            sidecars: config.sidecars,
            prices,
            settings,
            spawner,
            local_models: config.local_models,
            secret_jobs,
            bg_tx: bg_tx.clone(),
            modes,
            store,
            notices: notices.clone(),
            onboarding,
            snapshot: initial,
            snap_tx,
            calendar,
            account: (account.clone(), account_applies),
            cloud_credentials,
        });
        tokio::spawn(core.run(intents_rx, stream_rx, bg_rx));
        Ok(AppHandle {
            intents: intents_tx,
            snapshots: snap_rx,
            notices,
            bg: bg_tx,
            account,
            #[cfg(test)]
            stream_tx,
        })
    }
}

#[derive(Clone)]
pub struct AppHandle {
    intents: mpsc::Sender<Control>,
    snapshots: watch::Receiver<AppSnapshot>,
    notices: broadcast::Sender<Notice>,
    bg: mpsc::UnboundedSender<settings::BgMsg>,
    account: AccountHandle,
    #[cfg(test)]
    stream_tx: mpsc::UnboundedSender<streams::StreamMsg>,
}

impl std::fmt::Debug for AppHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppHandle").finish_non_exhaustive()
    }
}

impl AppHandle {
    /// Validates the intent and queues it. Never waits; a full queue is reported, not dropped.
    pub fn dispatch(&self, intent: Intent) -> Result<(), DispatchError> {
        intent.validate().map_err(DispatchError::Invalid)?;
        self.intents
            .try_send(Control::Intent(intent))
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => DispatchError::Busy,
                mpsc::error::TrySendError::Closed(_) => DispatchError::Stopped,
            })
    }

    /// Adds files the user picked in the shell's file panel to a mode. The core checks type,
    /// size and limits and reads them off the event loop; problems arrive as notices.
    pub fn add_mode_files(
        &self,
        mode_id: uuid::Uuid,
        paths: Vec<PathBuf>,
    ) -> Result<(), DispatchError> {
        self.bg
            .send(settings::BgMsg::Modes(modes::ModesMsg::AddPaths {
                mode_id,
                paths,
            }))
            .map_err(|_| DispatchError::Stopped)
    }
    /// Hands a `sotto://` link the OS delivered to the core. Only a sign-in callback that matches
    /// a login this process started has any effect.
    pub fn deep_link(&self, url: &str) {
        self.account.deep_link(url);
    }

    /// The latest snapshot, updated after every state change.
    #[must_use]
    pub fn snapshots(&self) -> watch::Receiver<AppSnapshot> {
        self.snapshots.clone()
    }

    #[must_use]
    pub fn notices(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    /// Ends any session, stops the sidecar and waits until pending history writes are on disk.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.intents.send(Control::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}
