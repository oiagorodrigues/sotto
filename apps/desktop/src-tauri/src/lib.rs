mod backend;
mod commands;
#[cfg(test)]
mod config_tests;
mod core;
mod logging;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(feature = "demo"))]
mod opener;
mod pick;
mod privacy;
#[cfg(not(feature = "demo"))]
mod real_core;
mod shell;

use std::sync::Arc;

use tauri::Manager;

use crate::commands::CoreState;
use crate::core::CoreHandle;

#[cfg(feature = "demo")]
fn start_core(_app: &tauri::App) -> Result<Arc<dyn CoreHandle>, Box<dyn std::error::Error>> {
    Ok(Arc::new(crate::core::DemoCore::new()))
}

#[cfg(not(feature = "demo"))]
fn start_core(app: &tauri::App) -> Result<Arc<dyn CoreHandle>, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    Ok(Arc::new(real_core::RealCore::start(&data_dir)?))
}

/// Keeps the log writer's flush thread alive for the life of the app.
#[derive(Debug)]
struct LogGuard {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Only `sotto://auth/callback` links reach the core, and the webview never sees them. The log
/// has the scheme at most, because the query carries a one-time code.
fn forward_deep_links(handle: &tauri::AppHandle, urls: &[tauri::Url]) {
    for url in urls {
        if backend::is_auth_callback(url) {
            handle.state::<CoreState>().0.deep_link(url.as_str());
        } else {
            tracing::info!(
                scheme = url.scheme(),
                "ignored a link the system opened with Sotto"
            );
        }
    }
}

pub fn run() {
    let built = tauri::Builder::default()
        .manage(shell::ShellState::default())
        .invoke_handler(tauri::generate_handler![
            commands::dispatch,
            commands::current_snapshot,
            commands::report_notch_size
        ])
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                shell::replay_last(webview.app_handle());
            }
        })
        .setup(|app| {
            if let Ok(dir) = app.path().app_log_dir()
                && let Some(guard) = logging::init(&dir)
            {
                app.manage(LogGuard { _guard: guard });
            }
            let core = start_core(app)?;
            app.manage(CoreState(core));
            shell::setup(app)
        })
        .build(tauri::generate_context!());
    match built {
        Ok(app) => app.run(|handle, event| match event {
            tauri::RunEvent::Exit => handle.state::<CoreState>().0.shutdown(),
            // The notch is hidden during onboarding, so the Dock icon is the way back to it.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => shell::on_reopen(handle),
            tauri::RunEvent::Opened { urls } => forward_deep_links(handle, &urls),
            _ => {}
        }),
        Err(e) => eprintln!("sotto: {e}"),
    }
}
