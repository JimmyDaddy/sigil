mod commands;
mod ipc;
mod recent;
mod run_streams;
mod startup;
mod state;

pub use startup::{clear_startup_failure, record_startup_failure, record_startup_panic};

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

use crate::{
    commands::{
        desktop_attach_run, desktop_bootstrap, desktop_cancel_run, desktop_catalog,
        desktop_close_workspace, desktop_create_session, desktop_open_recent_workspace,
        desktop_open_session, desktop_pick_workspace, desktop_rerun_verification,
        desktop_resolve_approval, desktop_start_run, desktop_transcript, desktop_verification,
        resolve_sigil_binary,
    },
    state::DesktopAppState,
};

const EXIT_IDLE: u8 = 0;
const EXIT_CLEANING: u8 = 1;
const EXIT_ALLOWED: u8 = 2;

/// Builds and runs the native shell while preserving workspace process ownership on exit.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let sigil_binary = resolve_sigil_binary()?;
    let exit_state = Arc::new(AtomicU8::new(EXIT_IDLE));
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Build the single native window here so development and package
            // overlays cannot diverge on whether the capability-labelled
            // `main` webview exists.
            WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Sigil")
                .inner_size(1120.0, 760.0)
                .min_inner_size(720.0, 560.0)
                .build()?;
            let recent_workspaces_path = app
                .path()
                .app_config_dir()?
                .join("recent-workspaces-v1.json");
            app.manage(DesktopAppState::new(
                sigil_binary.clone(),
                recent_workspaces_path,
            ));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap,
            desktop_pick_workspace,
            desktop_open_recent_workspace,
            desktop_close_workspace,
            desktop_catalog,
            desktop_create_session,
            desktop_open_session,
            desktop_transcript,
            desktop_start_run,
            desktop_attach_run,
            desktop_cancel_run,
            desktop_resolve_approval,
            desktop_verification,
            desktop_rerun_verification
        ])
        .build(tauri::generate_context!())?;

    app.run(move |app_handle, event| {
        let RunEvent::ExitRequested { api, .. } = event else {
            return;
        };
        match exit_state.load(Ordering::Acquire) {
            EXIT_ALLOWED => {}
            EXIT_CLEANING => api.prevent_exit(),
            _ => {
                api.prevent_exit();
                exit_state.store(EXIT_CLEANING, Ordering::Release);
                let handle = app_handle.clone();
                let state = app_handle.state::<DesktopAppState>();
                let manager = Arc::clone(&state.manager);
                let streams = state.run_streams.clone();
                let exit_state = Arc::clone(&exit_state);
                tauri::async_runtime::spawn(async move {
                    streams.stop_all().await;
                    manager.lock().await.close_all().await;
                    exit_state.store(EXIT_ALLOWED, Ordering::Release);
                    handle.exit(0);
                });
            }
        }
    });
    Ok(())
}
