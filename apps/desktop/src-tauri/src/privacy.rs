//! Opens the System Settings pane for a permission. The webview only names the permission; the
//! URLs are fixed here, so no text from the webview ever reaches the opener.

use sotto_protocol::sidecar::PermissionKind;
use tauri::AppHandle;

const PANE_PREFIX: &str = "x-apple.systempreferences:com.apple.preference.security?";

pub fn pane_url(permission: PermissionKind) -> String {
    let pane = match permission {
        PermissionKind::Microphone => "Privacy_Microphone",
        PermissionKind::SystemAudio => "Privacy_AudioCapture",
        PermissionKind::SpeechRecognition => "Privacy_SpeechRecognition",
        PermissionKind::ScreenRecording => "Privacy_ScreenCapture",
    };
    format!("{PANE_PREFIX}{pane}")
}

/// Runs on the main thread because the opener is AppKit.
pub fn open(app: &AppHandle, permission: PermissionKind) -> Result<(), String> {
    let url = pane_url(permission);
    app.run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        if let Err(e) = crate::macos::open_url(&url) {
            tracing::warn!(error = %e, "could not open System Settings");
        }
        #[cfg(not(target_os = "macos"))]
        let _ = url;
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_permission_has_its_own_pane() {
        let all = [
            PermissionKind::Microphone,
            PermissionKind::SystemAudio,
            PermissionKind::SpeechRecognition,
            PermissionKind::ScreenRecording,
        ];
        let urls: Vec<_> = all.into_iter().map(pane_url).collect();
        for url in &urls {
            assert!(url.starts_with("x-apple.systempreferences:"), "{url}");
            assert!(url.is_ascii() && !url.contains(' '));
        }
        let mut unique = urls.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), urls.len());
    }
}
