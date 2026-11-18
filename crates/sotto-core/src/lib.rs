//! App state, sessions, answer orchestration, providers, Keychain, storage and the sidecar process.

pub mod account;
pub mod app;
pub mod calendar;
pub mod cloud_stt;
pub mod pricing;
pub mod prompt;
pub mod providers;
pub mod secrets;
pub mod sidecar;
pub mod storage;

pub use app::{
    App, AppConfig, AppHandle, DefaultProviderFactory, DispatchError, LocalModels, Notice,
    NoticeKind, OllamaCatalog, ProcessSidecarFactory, Settings, detect_system_locale,
};
