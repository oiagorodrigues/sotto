//! Windows, placement and the effects that follow a snapshot.

use std::sync::{Arc, Mutex, PoisonError};

use sotto_protocol::ui::{AppSnapshot, Intent, Phase};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewWindow, WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::commands::CoreState;
use crate::core::CoreHandle;

pub const NOTCH: &str = "notch";
pub const SETTINGS: &str = "settings";
pub const ONBOARDING: &str = "onboarding";

pub const SNAPSHOT_EVENT: &str = "sotto://snapshot";
/// Shell-originated hint for the notch webview, for global shortcuts that need it (`ask`,
/// `toggle`).
pub const SHORTCUT_EVENT: &str = "sotto://shortcut";

const NOTCH_MIN_H: f64 = 32.0;
/// Bounds for what the webview may ask the notch window to be. Wide enough for the 760 px panel
/// plus its ears; the height cap is a share of the screen so a bad report cannot cover it.
const MIN_WIDTH: f64 = 200.0;
const MAX_WIDTH: f64 = 820.0;
const MIN_HEIGHT: f64 = 24.0;
const MAX_HEIGHT_SHARE: f64 = 0.75;

/// What the webview needs to know about the hardware notch. Zero width means no notch.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct NotchGeometry {
    pub notch_width: f64,
    pub notch_height: f64,
}

#[derive(Debug, Default)]
pub struct ShellState {
    last: Mutex<Option<AppSnapshot>>,
    geometry: Mutex<Option<NotchGeometry>>,
}

pub fn setup(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    place_notch(&handle);
    register_shortcuts(&handle)?;

    for label in [SETTINGS, ONBOARDING] {
        if let Some(w) = app.get_webview_window(label) {
            let hidden = w.clone();
            w.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = hidden.hide();
                }
            });
        }
    }

    let sink_handle = handle.clone();
    let core = app.state::<CoreState>().0.clone();
    core.subscribe(Arc::new(move |snapshot| publish(&sink_handle, snapshot)))?;
    Ok(())
}

/// Latest snapshot, for a webview that subscribed after it was emitted.
pub fn last_snapshot(app: &AppHandle) -> Option<AppSnapshot> {
    app.state::<ShellState>()
        .last
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Sends the last snapshot again, for a webview that finished loading after it was emitted.
pub fn replay_last(app: &AppHandle) {
    if let Some(snapshot) = last_snapshot(app) {
        let _ = app.emit(SNAPSHOT_EVENT, snapshot);
    }
}

fn publish(app: &AppHandle, snapshot: AppSnapshot) {
    let state = app.state::<ShellState>();
    let previous = state
        .last
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .replace(snapshot.clone());
    let _ = app.emit(SNAPSHOT_EVENT, &snapshot);

    if previous.as_ref().map(|p| p.invisible) != Some(snapshot.invisible) {
        apply_capture_hidden(app, snapshot.invisible);
    }
    let was_onboarding = previous.as_ref().map(|p| p.phase) == Some(Phase::Onboarding);
    let is_onboarding = snapshot.phase == Phase::Onboarding;
    if is_onboarding && !was_onboarding {
        show_window(app, ONBOARDING);
        set_notch_visible(app, false);
    } else if was_onboarding && !is_onboarding {
        hide_window(app, ONBOARDING);
        set_notch_visible(app, true);
    }
}

pub fn hide_window(app: &AppHandle, label: &'static str) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.hide();
        }
    });
}

fn set_notch_visible(app: &AppHandle, visible: bool) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window(NOTCH) {
            let _ = if visible { w.show() } else { w.hide() };
        }
    });
}

/// Dock icon click: during first-run onboarding the window may have been closed.
pub fn on_reopen(app: &AppHandle) {
    if last_snapshot(app).map(|s| s.phase) == Some(Phase::Onboarding) {
        show_window(app, ONBOARDING);
    }
}

pub fn show_window(app: &AppHandle, label: &'static str) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    });
}

/// The panel must be key before the webview focuses its input, or typing goes to the frontmost
/// app.
fn focus_notch_then_ask(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window(NOTCH) {
            #[cfg(target_os = "macos")]
            let _ = crate::macos::make_key_and_front(&w);
            #[cfg(not(target_os = "macos"))]
            let _ = w.set_focus();
        }
        let _ = app.emit_to(NOTCH, SHORTCUT_EVENT, "ask");
    });
}

fn toggle_expanded(app: &AppHandle) {
    let _ = app.emit_to(NOTCH, SHORTCUT_EVENT, "toggle");
}

fn toggle_pill_visibility(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window(NOTCH) {
            if w.is_visible().unwrap_or(false) {
                let _ = w.hide();
            } else {
                let _ = w.show();
            }
        }
    });
}

fn apply_capture_hidden(app: &AppHandle, hidden: bool) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window(NOTCH) {
            let _ = set_capture_hidden(&w, hidden);
        }
    });
}

/// Hides `window` from screen capture (macOS `NSWindow.sharingType`). No-op elsewhere.
pub fn set_capture_hidden(window: &WebviewWindow, hidden: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::set_capture_hidden(window, hidden)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, hidden);
        Ok(())
    }
}

/// Must run on the main thread (AppKit).
fn measure_notch() -> NotchGeometry {
    #[cfg(target_os = "macos")]
    if let Some(n) = crate::macos::notch_size() {
        return NotchGeometry {
            notch_width: n.width,
            notch_height: n.height,
        };
    }
    NotchGeometry {
        notch_width: 0.0,
        notch_height: NOTCH_MIN_H,
    }
}

fn store_geometry(app: &AppHandle) -> NotchGeometry {
    let geometry = measure_notch();
    *app.state::<ShellState>()
        .geometry
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(geometry);
    geometry
}

fn place_notch(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let geometry = store_geometry(&app);
        if let Some(notch) = app.get_webview_window(NOTCH) {
            #[cfg(target_os = "macos")]
            let _ = crate::macos::make_panel(&notch);
            let _ = set_capture_hidden(&notch, true);
            let compact_width = if geometry.notch_width > 0.0 {
                geometry.notch_width + 240.0
            } else {
                232.0
            };
            let (width, height) =
                clamp_size(compact_width, geometry.notch_height, f64::MAX, f64::MAX);
            apply_frame(&notch, width, height);
            let _ = notch.show();
        }
    });
}

/// Keeps a reported size inside what the shell is willing to show.
fn clamp_size(width: f64, height: f64, screen_width: f64, screen_height: f64) -> (f64, f64) {
    let finite = |v: f64, fallback: f64| if v.is_finite() { v } else { fallback };
    let max_w = MAX_WIDTH.min(screen_width);
    let max_h = (screen_height * MAX_HEIGHT_SHARE).max(MIN_HEIGHT);
    (
        finite(width, MIN_WIDTH).clamp(MIN_WIDTH, max_w.max(MIN_WIDTH)),
        finite(height, MIN_HEIGHT).clamp(MIN_HEIGHT, max_h),
    )
}

/// Top edge on the screen top, horizontally centered on the notch (the screen center).
fn apply_frame(notch: &WebviewWindow, width: f64, height: f64) {
    #[cfg(target_os = "macos")]
    if crate::macos::place_top_centered(notch, width, height).is_ok() {
        return;
    }
    let _ = notch.set_size(LogicalSize::new(width, height));
    if let Ok(Some(monitor)) = notch.primary_monitor() {
        let scale = monitor.scale_factor();
        let x = f64::from(monitor.position().x)
            + (f64::from(monitor.size().width) - width * scale) / 2.0;
        let _ = notch.set_position(PhysicalPosition::new(x, f64::from(monitor.position().y)));
    }
}

/// Resizes the notch window to the content size the webview measured. Returns the hardware notch
/// geometry so the webview can lay out around it without any script injection.
pub fn resize_notch(app: &AppHandle, width: f64, height: f64) -> NotchGeometry {
    let geometry = app
        .state::<ShellState>()
        .geometry
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unwrap_or(NotchGeometry {
            notch_width: 0.0,
            notch_height: NOTCH_MIN_H,
        });
    let (screen_w, screen_h) = app
        .get_webview_window(NOTCH)
        .and_then(|w| w.primary_monitor().ok().flatten())
        .map_or((MAX_WIDTH, f64::from(u16::MAX)), |m| {
            let scale = m.scale_factor();
            (
                f64::from(m.size().width) / scale,
                f64::from(m.size().height) / scale,
            )
        });
    let (width, height) = clamp_size(width, height, screen_w, screen_h);
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        store_geometry(&handle);
        if let Some(notch) = handle.get_webview_window(NOTCH) {
            apply_frame(&notch, width, height);
        }
    });
    geometry
}

fn register_shortcuts(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let ask = Shortcut::new(Some(Modifiers::SUPER), Code::Enter);
    let toggle = Shortcut::new(Some(Modifiers::SUPER), Code::Backslash);
    let listen = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyL);
    let invisible = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyI);
    let hide = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyH);
    app.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_handler(move |app, shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                if shortcut == &ask {
                    focus_notch_then_ask(app);
                } else if shortcut == &toggle {
                    toggle_expanded(app);
                } else if shortcut == &listen {
                    toggle_listening(app);
                } else if shortcut == &invisible {
                    toggle_invisible(app);
                } else if shortcut == &hide {
                    toggle_pill_visibility(app);
                }
            })
            .build(),
    )?;
    let shortcuts = app.global_shortcut();
    for s in [ask, toggle, listen, invisible, hide] {
        // Another app may own the combination; the shell still works without it.
        let _ = shortcuts.register(s);
    }
    Ok(())
}

fn toggle_listening(app: &AppHandle) {
    let phase = last_snapshot(app).map(|s| s.phase);
    let intent = match phase {
        Some(Phase::Listening) => Intent::EndSession {},
        Some(Phase::Idle) => Intent::StartSession { mode_id: None },
        _ => return,
    };
    let core: Arc<dyn CoreHandle> = app.state::<CoreState>().0.clone();
    let _ = core.dispatch(intent);
}

fn toggle_invisible(app: &AppHandle) {
    if let Some(invisible) = last_snapshot(app).map(|s| s.invisible) {
        let core: Arc<dyn CoreHandle> = app.state::<CoreState>().0.clone();
        let _ = core.dispatch(Intent::SetInvisible {
            invisible: !invisible,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_sizes_are_clamped_to_sane_bounds() {
        assert_eq!(clamp_size(600.0, 400.0, 1728.0, 1117.0), (600.0, 400.0));
        assert_eq!(clamp_size(5000.0, 50.0, 1728.0, 1117.0), (MAX_WIDTH, 50.0));
        assert_eq!(
            clamp_size(1.0, 1.0, 1728.0, 1117.0),
            (MIN_WIDTH, MIN_HEIGHT)
        );
        let (_, h) = clamp_size(600.0, 5000.0, 1728.0, 1000.0);
        assert!((h - 750.0).abs() < f64::EPSILON);
    }

    #[test]
    fn non_finite_sizes_fall_back_to_the_minimum() {
        assert_eq!(
            clamp_size(f64::NAN, f64::INFINITY, 1728.0, 1000.0),
            (MIN_WIDTH, MIN_HEIGHT)
        );
    }

    #[test]
    fn narrow_screens_cap_the_width() {
        assert_eq!(clamp_size(800.0, 100.0, 700.0, 800.0), (700.0, 100.0));
    }
}
