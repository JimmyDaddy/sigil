mod appearance;
mod commands;
mod ipc;
mod recent;
mod run_streams;
mod startup;
mod state;
mod update;
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
        desktop_accept_task_integration, desktop_agent_activity, desktop_attach_run,
        desktop_bootstrap, desktop_cancel_run, desktop_cancel_terminal_task, desktop_catalog,
        desktop_checkpoint_restore_preview, desktop_close_workspace,
        desktop_command_conversation_queue, desktop_command_conversation_recovery,
        desktop_compact_conversation, desktop_continue_task, desktop_continuity,
        desktop_conversation_compaction_preview, desktop_conversation_queue,
        desktop_conversation_recovery, desktop_create_session,
        desktop_delete_invalid_session_source, desktop_delete_session, desktop_display,
        desktop_execute_intent_drop, desktop_execute_session_catalog_batch,
        desktop_export_support_bundle, desktop_intent_stack, desktop_open_external_url,
        desktop_open_recent_workspace, desktop_open_session, desktop_pause_task,
        desktop_pick_workspace, desktop_plan_decision, desktop_plan_session_catalog_batch,
        desktop_preview_intent_drop, desktop_provider_connections, desktop_provider_setup_catalog,
        desktop_quarantine_session, desktop_read_tool_artifact, desktop_rename_session,
        desktop_rerun_verification, desktop_resolve_approval, desktop_run_context,
        desktop_save_provider_default_model, desktop_save_provider_setup, desktop_set_appearance,
        desktop_start_run, desktop_support_doctor, desktop_task_integration_review,
        desktop_transcript, desktop_verification, resolve_sigil_binary,
    },
    state::DesktopAppState,
    update::{
        DesktopUpdaterState, desktop_check_for_update, desktop_download_and_install_update,
        desktop_restart_after_update, desktop_update_state,
    },
    window_state::{DisplayBounds, WindowGeometry, WindowStateOwner},
};

const EXIT_IDLE: u8 = 0;
const EXIT_CLEANING: u8 = 1;
const EXIT_ALLOWED: u8 = 2;

#[derive(Clone)]
pub(crate) struct DesktopExitState {
    value: Arc<AtomicU8>,
}

impl DesktopExitState {
    fn new() -> Self {
        Self {
            value: Arc::new(AtomicU8::new(EXIT_IDLE)),
        }
    }

    fn load(&self) -> u8 {
        self.value.load(Ordering::Acquire)
    }

    pub(crate) fn begin_cleanup(&self) -> bool {
        self.value
            .compare_exchange(
                EXIT_IDLE,
                EXIT_CLEANING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn cancel_cleanup(&self) {
        self.value.store(EXIT_IDLE, Ordering::Release);
    }

    pub(crate) fn allow_exit(&self) {
        self.value.store(EXIT_ALLOWED, Ordering::Release);
    }
}

/// Builds and runs the native shell while preserving workspace process ownership on exit.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let sigil_binary = resolve_sigil_binary()?;
    let exit_state = DesktopExitState::new();
    let setup_exit_state = exit_state.clone();
    let builder = tauri::Builder::default();
    #[cfg(feature = "desktop-e2e")]
    let builder = builder
        .plugin(tauri_plugin_wdio::init())
        .plugin(tauri_plugin_wdio_webdriver::init());
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            let updater = DesktopUpdaterState::new(
                app.package_info().version.to_string(),
                config_dir.join("desktop-update-last-check-v1.json"),
            );
            app.manage(updater.clone());
            updater.start_background_check(app.handle().clone());
            app.manage(setup_exit_state.clone());
            app.manage(window_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap,
            desktop_open_external_url,
            desktop_support_doctor,
            desktop_export_support_bundle,
            desktop_provider_connections,
            desktop_provider_setup_catalog,
            desktop_save_provider_default_model,
            desktop_save_provider_setup,
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
            desktop_display,
            desktop_read_tool_artifact,
            desktop_continuity,
            desktop_conversation_queue,
            desktop_command_conversation_queue,
            desktop_conversation_recovery,
            desktop_conversation_compaction_preview,
            desktop_compact_conversation,
            desktop_checkpoint_restore_preview,
            desktop_command_conversation_recovery,
            desktop_run_context,
            desktop_agent_activity,
            desktop_start_run,
            desktop_continue_task,
            desktop_attach_run,
            desktop_cancel_run,
            desktop_cancel_terminal_task,
            desktop_pause_task,
            desktop_plan_decision,
            desktop_resolve_approval,
            desktop_verification,
            desktop_rerun_verification,
            desktop_task_integration_review,
            desktop_accept_task_integration,
            desktop_intent_stack,
            desktop_preview_intent_drop,
            desktop_execute_intent_drop,
            desktop_set_appearance,
            desktop_update_state,
            desktop_check_for_update,
            desktop_download_and_install_update,
            desktop_restart_after_update
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
        RunEvent::ExitRequested { api, .. } => match exit_state.load() {
            EXIT_ALLOWED => {}
            EXIT_CLEANING => api.prevent_exit(),
            _ => {
                persist_window_state(app_handle);
                api.prevent_exit();
                if !exit_state.begin_cleanup() {
                    return;
                }
                let handle = (*app_handle).clone();
                let state = app_handle.state::<DesktopAppState>();
                let manager = Arc::clone(&state.manager);
                let streams = state.run_streams.clone();
                let exit_state = exit_state.clone();
                let updater = app_handle.state::<DesktopUpdaterState>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    updater.stop_background();
                    streams.stop_all().await;
                    manager.lock().await.close_all().await;
                    exit_state.allow_exit();
                    handle.exit(0);
                });
            }
        },
        _ => {}
    });
    Ok(())
}

pub(crate) fn persist_window_state(app_handle: &tauri::AppHandle) {
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
