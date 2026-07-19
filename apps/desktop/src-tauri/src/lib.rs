mod commands;
mod state;

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use tauri::RunEvent;

use crate::{
    commands::{
        desktop_bootstrap, desktop_close_workspace, desktop_pick_workspace, resolve_sigil_binary,
    },
    state::DesktopAppState,
};

const EXIT_IDLE: u8 = 0;
const EXIT_CLEANING: u8 = 1;
const EXIT_ALLOWED: u8 = 2;

/// Builds and runs the native shell while preserving workspace process ownership on exit.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let state = DesktopAppState::new(resolve_sigil_binary()?);
    let shutdown_manager = Arc::clone(&state.manager);
    let exit_state = Arc::new(AtomicU8::new(EXIT_IDLE));
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap,
            desktop_pick_workspace,
            desktop_close_workspace
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
                let manager = Arc::clone(&shutdown_manager);
                let exit_state = Arc::clone(&exit_state);
                tauri::async_runtime::spawn(async move {
                    manager.lock().await.close_all().await;
                    exit_state.store(EXIT_ALLOWED, Ordering::Release);
                    handle.exit(0);
                });
            }
        }
    });
    Ok(())
}
