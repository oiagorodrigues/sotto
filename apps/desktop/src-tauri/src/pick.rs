//! The native file panel for mode files. It opens from Rust, in answer to a `PickModeFiles`
//! intent, and the chosen paths go straight to the core: the webview never sees a path and no
//! dialog permission is granted to JavaScript.

use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use crate::core::CoreHandle;

const FILE_TYPES: [&str; 3] = ["txt", "md", "pdf"];

async fn choose() -> Vec<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Add files to this mode")
        .add_filter("Text, Markdown and PDF", &FILE_TYPES)
        .pick_files()
        .await
        .map(|files| files.iter().map(|f| f.path().to_path_buf()).collect())
        .unwrap_or_default()
}

/// Opens the panel and gives the chosen files to the mode. The core checks type and size again.
pub fn pick_for_mode(core: Arc<dyn CoreHandle>, mode_id: Uuid) {
    tauri::async_runtime::spawn(async move {
        let paths = choose().await;
        if paths.is_empty() {
            return;
        }
        if let Err(e) = core.add_mode_files(mode_id, paths) {
            tracing::warn!(error = %e, "could not hand the chosen files to the core");
        }
    });
}
