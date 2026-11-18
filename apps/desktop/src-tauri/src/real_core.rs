//! Wires `sotto-core` into the shell: data directory, Keychain-held database key, sidecar path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sotto_core::account::{AccountConfig, AccountError};
use sotto_core::calendar::CalendarConfig;
use sotto_core::pricing::{PriceTable, PricingError};
use sotto_core::secrets::{Account, KeychainStore, SecretStore, SecretsError};
use sotto_core::storage::{Db, StorageError};
use sotto_core::{
    App, AppConfig, AppHandle, DefaultProviderFactory, OllamaCatalog, ProcessSidecarFactory,
    Settings,
};
use sotto_protocol::ui::Intent;
use uuid::Uuid;

use crate::core::{CoreHandle, SnapshotSink};

const DB_FILE: &str = "sotto.db";
const SIDECAR_NAME: &str = "sotto-audio";
const DEBUG_SIDECAR_FILE: &str = "sotto-audio-aarch64-apple-darwin";
const DB_KEY_BYTES: usize = 32;
const CLIPS_DIR: &str = "clips";

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("cannot prepare the data directory {0}: {1}")]
    DataDir(PathBuf, std::io::Error),
    #[error("Keychain: {0}")]
    Keychain(#[from] SecretsError),
    #[error(
        "the database key in the Keychain is damaged; it is not replaced because that would orphan the history"
    )]
    DamagedDbKey,
    #[error("cannot generate the database key: {0}")]
    Random(getrandom::Error),
    #[error("cannot open the database: {0}")]
    Database(#[from] StorageError),
    #[error("pricing table: {0}")]
    Pricing(#[from] PricingError),
    #[error("cannot start the core: {0}")]
    Core(#[from] sotto_core::app::StartError),
}

pub struct RealCore {
    handle: AppHandle,
}

impl std::fmt::Debug for RealCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealCore").finish_non_exhaustive()
    }
}

impl RealCore {
    /// Starts the core on Tauri's tokio runtime.
    pub fn start(data_dir: &Path) -> Result<Self, StartupError> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| StartupError::DataDir(data_dir.to_path_buf(), e))?;
        let secrets: Arc<dyn SecretStore> = Arc::new(KeychainStore);
        let db_key = db_key(secrets.as_ref())?;
        let db = Db::open(&data_dir.join(DB_FILE), db_key)?;
        let exe = std::env::current_exe().unwrap_or_default();
        let sidecar = sidecar_path(&exe);
        let config = AppConfig {
            sidecars: Arc::new(ProcessSidecarFactory::new(sidecar)),
            secrets,
            db,
            prices: PriceTable::bundled()?,
            providers: Arc::new(DefaultProviderFactory::new()),
            local_models: Arc::new(OllamaCatalog),
            settings: Settings {
                require_onboarding: true,
                clips_dir: Some(clips_dir(&exe)),
                ..Settings::default()
            },
            data_dir: Some(data_dir.to_path_buf()),
            calendar: CalendarConfig::google(Arc::new(crate::opener::SystemBrowser)),
            embedder: None,
            account: account_config(),
        };
        let handle = tauri::async_runtime::block_on(async { App::start(config) })?;
        Ok(Self { handle })
    }
}

impl CoreHandle for RealCore {
    fn dispatch(&self, intent: Intent) -> Result<(), String> {
        self.handle.dispatch(intent).map_err(|e| e.to_string())
    }

    fn subscribe(&self, sink: SnapshotSink) -> Result<(), String> {
        let mut snapshots = self.handle.snapshots();
        tauri::async_runtime::spawn(async move {
            sink(snapshots.borrow_and_update().clone());
            while snapshots.changed().await.is_ok() {
                sink(snapshots.borrow_and_update().clone());
            }
        });
        Ok(())
    }

    fn add_mode_files(&self, mode_id: Uuid, paths: Vec<PathBuf>) -> Result<(), String> {
        self.handle
            .add_mode_files(mode_id, paths)
            .map_err(|e| e.to_string())
    }

    fn shutdown(&self) {
        tauri::async_runtime::block_on(self.handle.shutdown());
    }

    fn deep_link(&self, url: &str) {
        self.handle.deep_link(url);
    }
}

/// Sign-in is off when the build has no valid backend address.
fn account_config() -> AccountConfig {
    let Some(url) = crate::backend::backend_url() else {
        return AccountConfig::disabled();
    };
    let result = sotto_core::account::BackendUrl::parse(url).and_then(|backend| {
        let opener = crate::backend::BackendBrowser::new(backend.origin());
        AccountConfig::enabled(url, Arc::new(opener))
    });
    result.unwrap_or_else(|e: AccountError| {
        tracing::warn!(error = %e, "sign-in is disabled");
        AccountConfig::disabled()
    })
}

/// Reads the database key from the Keychain, or creates it on first run. A key that is present
/// but unreadable is an error: replacing it would make the existing history unreadable.
pub fn db_key(secrets: &dyn SecretStore) -> Result<[u8; DB_KEY_BYTES], StartupError> {
    if let Some(stored) = secrets.get(Account::DbKey)? {
        return decode_key(&stored).ok_or(StartupError::DamagedDbKey);
    }
    let mut key = [0_u8; DB_KEY_BYTES];
    getrandom::fill(&mut key).map_err(StartupError::Random)?;
    secrets.set(Account::DbKey, &encode_key(&key))?;
    Ok(key)
}

fn encode_key(key: &[u8; DB_KEY_BYTES]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_key(hex: &str) -> Option<[u8; DB_KEY_BYTES]> {
    if hex.len() != DB_KEY_BYTES * 2 || !hex.is_ascii() {
        return None;
    }
    let mut key = [0_u8; DB_KEY_BYTES];
    for (byte, pair) in key.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// Debug builds run the copy `just build-sidecar` leaves in this crate's `binaries/` directory,
/// found through the compile-time manifest path; release builds use the copy Tauri bundles next
/// to the app binary. Never searched through `PATH` or environment variables.
pub fn sidecar_path(current_exe: &Path) -> PathBuf {
    if cfg!(debug_assertions) {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(DEBUG_SIDECAR_FILE)
    } else {
        current_exe
            .parent()
            .unwrap_or(Path::new("."))
            .join(SIDECAR_NAME)
    }
}

/// The bundled sample clips for the transcription check. Debug builds read them from the
/// repository; release builds from the app bundle's `Resources/clips`.
pub fn clips_dir(current_exe: &Path) -> PathBuf {
    if cfg!(debug_assertions) {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/audio")
    } else {
        current_exe
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."))
            .join("Resources")
            .join(CLIPS_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sotto_core::secrets::MemoryStore;

    #[test]
    fn the_database_key_is_created_once_and_reused() {
        let store = MemoryStore::new();
        let first = db_key(&store).unwrap();
        assert_ne!(first, [0_u8; DB_KEY_BYTES]);
        assert_eq!(db_key(&store).unwrap(), first);
        let stored = store.get(Account::DbKey).unwrap().unwrap();
        assert_eq!(stored.len(), 64);
        assert_eq!(decode_key(&stored), Some(first));
    }

    #[test]
    fn a_damaged_database_key_is_never_replaced() {
        let store = MemoryStore::new();
        store.set(Account::DbKey, "not hex").unwrap();
        assert!(matches!(db_key(&store), Err(StartupError::DamagedDbKey)));
        assert_eq!(
            store.get(Account::DbKey).unwrap().as_deref(),
            Some("not hex")
        );
    }

    #[test]
    fn the_clips_directory_holds_the_bundled_clips() {
        let exe = Path::new("/Applications/Sotto.app/Contents/MacOS/sotto-desktop");
        let dir = clips_dir(exe);
        if cfg!(debug_assertions) {
            assert!(
                dir.join("pt-BR-question.wav").is_file(),
                "{}",
                dir.display()
            );
            assert!(dir.join("en-US-question.wav").is_file());
        } else {
            assert_eq!(
                dir,
                Path::new("/Applications/Sotto.app/Contents/Resources/clips")
            );
        }
    }

    #[test]
    fn the_sidecar_path_is_absolute() {
        let exe = Path::new("/Applications/Sotto.app/Contents/MacOS/sotto-desktop");
        let path = sidecar_path(exe);
        assert!(path.is_absolute());
        if cfg!(debug_assertions) {
            assert!(path.ends_with("src-tauri/binaries/sotto-audio-aarch64-apple-darwin"));
        } else {
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some("sotto-audio")
            );
            assert_eq!(
                path,
                Path::new("/Applications/Sotto.app/Contents/MacOS/sotto-audio")
            );
        }
    }
}
