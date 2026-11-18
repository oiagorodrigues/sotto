use std::sync::Arc;

use sotto_protocol::ui::{AppSnapshot, Intent, Phase};
use tauri::{AppHandle, State};

use crate::core::CoreHandle;
use crate::{privacy, shell};

pub struct CoreState(pub Arc<dyn CoreHandle>);

impl std::fmt::Debug for CoreState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreState").finish_non_exhaustive()
    }
}

/// Validates an intent from the webview and forwards it to the core. Shell-only effects (opening
/// the settings window, the native file panel) happen here because the core does not own windows.
pub fn handle_intent(core: &dyn CoreHandle, intent: Intent) -> Result<(), String> {
    intent
        .validate()
        .map_err(|e| format!("invalid intent: {e:?}"))?;
    core.dispatch(intent)
}

#[tauri::command]
pub fn dispatch(app: AppHandle, core: State<'_, CoreState>, intent: Intent) -> Result<(), String> {
    let open_settings = matches!(intent, Intent::OpenSettings {});
    let reopen_onboarding = matches!(intent, Intent::ReopenOnboarding {});
    let complete_onboarding = matches!(intent, Intent::CompleteOnboarding {});
    if let Intent::OpenPrivacySettings { permission } = &intent {
        intent
            .validate()
            .map_err(|e| format!("invalid intent: {e:?}"))?;
        return privacy::open(&app, *permission);
    }
    let was_onboarding = shell::last_snapshot(&app).map(|s| s.phase) == Some(Phase::Onboarding);
    let pick_for = match &intent {
        Intent::PickModeFiles { mode_id } => Some(*mode_id),
        _ => None,
    };
    handle_intent(core.0.as_ref(), intent)?;
    if open_settings {
        shell::show_window(&app, shell::SETTINGS);
    }
    if reopen_onboarding {
        shell::show_window(&app, shell::ONBOARDING);
    }
    // First-run completion hides the window from the snapshot change; a reopened window has no
    // phase change to react to.
    if complete_onboarding && !was_onboarding {
        shell::hide_window(&app, shell::ONBOARDING);
    }
    if let Some(mode_id) = pick_for {
        crate::pick::pick_for_mode(Arc::clone(&core.0), mode_id);
    }
    Ok(())
}

/// Read-only. The `sotto://snapshot` event alone loses the state emitted before the webview's
/// listener exists (an idle core emits nothing afterwards), so each view pulls it once after
/// subscribing.
#[tauri::command]
pub fn current_snapshot(app: AppHandle, webview: tauri::Webview) -> Option<AppSnapshot> {
    let snapshot = shell::last_snapshot(&app);
    tracing::debug!(
        window = webview.label(),
        present = snapshot.is_some(),
        "webview pulled the current snapshot"
    );
    snapshot
}

/// Sizes the notch window to the content the webview measured and returns the hardware notch
/// geometry. Only the notch webview may call it; the shell clamps the size.
#[tauri::command]
pub fn report_notch_size(
    app: AppHandle,
    webview: tauri::Webview,
    width: f64,
    height: f64,
) -> Result<shell::NotchGeometry, String> {
    if webview.label() != shell::NOTCH {
        return Err("only the notch window can report its size".into());
    }
    Ok(shell::resize_notch(&app, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DemoCore;

    #[test]
    fn rejects_an_empty_ask_before_it_reaches_the_core() {
        let core = DemoCore::new();
        let intent = Intent::Ask {
            text: String::new(),
            with_screenshot: false,
        };
        assert!(handle_intent(&core, intent).is_err());
    }

    #[test]
    fn rejects_an_invalid_mode_before_it_reaches_the_core() {
        let core = DemoCore::new();
        let intent = Intent::CreateMode {
            name: "  ".into(),
            system_prompt: String::new(),
        };
        assert!(handle_intent(&core, intent).is_err());
    }

    #[test]
    fn the_file_panel_intent_carries_no_path() {
        let core = DemoCore::new();
        let mode_id = uuid::Uuid::nil();
        assert!(handle_intent(&core, Intent::PickModeFiles { mode_id }).is_ok());
        let with_path =
            format!(r#"{{"type":"pick_mode_files","mode_id":"{mode_id}","path":"/etc/hosts"}}"#);
        assert!(serde_json::from_str::<Intent>(&with_path).is_err());
    }

    #[test]
    fn forwards_a_valid_intent() {
        let core = DemoCore::new();
        assert!(handle_intent(&core, Intent::CancelAnswer {}).is_ok());
    }
}
