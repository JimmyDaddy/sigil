mod appearance;
mod commands;
mod ipc;
mod recent;
mod run_streams;
mod startup;
mod state;
mod window_state;

pub use startup::{clear_startup_failure, record_startup_failure, record_startup_panic};

use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use tauri::{Emitter, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::{
    appearance::{
        AppearanceSnapshot, AppearanceStore, DESKTOP_APPEARANCE_EVENT_NAME, ResolvedTheme,
        ThemePreference, initialization_script,
    },
    commands::{
        desktop_agent_activity, desktop_attach_run, desktop_bootstrap, desktop_cancel_run,
        desktop_catalog, desktop_close_workspace, desktop_create_session,
        desktop_delete_invalid_session_source, desktop_delete_session,
        desktop_execute_session_catalog_batch, desktop_export_support_bundle,
        desktop_open_external_url, desktop_open_recent_workspace, desktop_open_session,
        desktop_pick_workspace, desktop_plan_session_catalog_batch, desktop_quarantine_session,
        desktop_rename_session, desktop_rerun_verification, desktop_resolve_approval,
        desktop_run_context, desktop_set_appearance, desktop_start_run, desktop_support_doctor,
        desktop_transcript, desktop_verification, resolve_sigil_binary,
    },
    state::DesktopAppState,
    window_state::{DisplayBounds, WindowGeometry, WindowStateOwner},
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
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .setup(move |app| {
            let config_dir = app.path().app_config_dir()?;
            let appearance = AppearanceStore::load(config_dir.join("appearance-v1.json"));
            let theme_preference = appearance.preference();
            let window_state = WindowStateOwner::load(config_dir.join("window-state-v1.json"));
            let displays = match app.available_monitors() {
                Ok(monitors) => monitors
                    .into_iter()
                    .map(|monitor| {
                        let area = monitor.work_area();
                        DisplayBounds {
                            x: area.position.x,
                            y: area.position.y,
                            width: area.size.width,
                            height: area.size.height,
                            scale_factor: monitor.scale_factor(),
                        }
                    })
                    .collect::<Vec<_>>(),
                Err(error) => {
                    eprintln!("sigil desktop: monitor discovery failed: {error}");
                    Vec::new()
                }
            };
            // Build the single native window here so development and package
            // overlays cannot diverge on whether the capability-labelled
            // `main` webview exists.
            let mut window_builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Sigil")
                .inner_size(1440.0, 900.0)
                .min_inner_size(1100.0, 720.0)
                .theme(theme_preference.native_theme())
                .initialization_script(initialization_script(theme_preference));
            if let Some(geometry) = window_state.initial_geometry(&displays) {
                window_builder = window_builder
                    .position(geometry.x, geometry.y)
                    .inner_size(geometry.width, geometry.height)
                    .maximized(geometry.maximized);
            }
            window_builder.build()?;
            let recent_workspaces_path = config_dir.join("recent-workspaces-v1.json");
            app.manage(DesktopAppState::new(
                sigil_binary.clone(),
                recent_workspaces_path,
                appearance,
            ));
            app.manage(window_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap,
            desktop_open_external_url,
            desktop_support_doctor,
            desktop_export_support_bundle,
            desktop_pick_workspace,
            desktop_open_recent_workspace,
            desktop_close_workspace,
            desktop_catalog,
            desktop_plan_session_catalog_batch,
            desktop_execute_session_catalog_batch,
            desktop_create_session,
            desktop_open_session,
            desktop_rename_session,
            desktop_delete_session,
            desktop_delete_invalid_session_source,
            desktop_quarantine_session,
            desktop_transcript,
            desktop_run_context,
            desktop_agent_activity,
            desktop_start_run,
            desktop_attach_run,
            desktop_cancel_run,
            desktop_resolve_approval,
            desktop_verification,
            desktop_rerun_verification,
            desktop_set_appearance
        ])
        .build(tauri::generate_context!())?;

    app.run(move |app_handle, event| match event {
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::Focused(false),
            ..
        } if label == "main" => persist_window_state(app_handle),
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { .. },
            ..
        } if label == "main" => persist_window_state(app_handle),
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::ThemeChanged(theme),
            ..
        } if label == "main" => {
            let state = app_handle.state::<DesktopAppState>();
            let preference = state
                .appearance
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .preference();
            if preference == ThemePreference::System {
                let _ = app_handle.emit(
                    DESKTOP_APPEARANCE_EVENT_NAME,
                    AppearanceSnapshot {
                        preference,
                        resolved_theme: ResolvedTheme::from(theme),
                    },
                );
            }
        }
        RunEvent::ExitRequested { api, .. } => match exit_state.load(Ordering::Acquire) {
            EXIT_ALLOWED => {}
            EXIT_CLEANING => api.prevent_exit(),
            _ => {
                persist_window_state(app_handle);
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
        },
        _ => {}
    });
    Ok(())
}

fn persist_window_state(app_handle: &tauri::AppHandle) {
    let Some(window) = app_handle.get_webview_window("main") else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.inner_size() else {
        return;
    };
    let owner = app_handle.state::<WindowStateOwner>();
    if let Err(error) = owner.persist(WindowGeometry {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized().unwrap_or(false),
    }) {
        eprintln!("sigil desktop: {error}");
    }
}
