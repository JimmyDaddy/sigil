#![cfg_attr(
    all(test, sigil_tui_test_slice_app_input_flow),
    allow(dead_code, unused_imports)
)]

#[cfg(test)]
#[macro_use]
#[path = "tests/mcp_config_macros.rs"]
mod mcp_config_macros;

#[cfg(test)]
#[path = "tests/test_env.rs"]
pub(crate) mod test_env;

pub(crate) mod agent_display;
pub(crate) mod app;
pub(crate) mod appearance_diagnostics;
#[cfg(not(test))]
pub(crate) mod application_bridge;
pub(crate) mod approval;
pub(crate) mod attention;
pub(crate) mod clipboard;
pub(crate) mod clipboard_image;
pub(crate) mod commands;
pub(crate) mod config_panel;
pub(crate) mod damage;
pub(crate) mod host_effects;
pub(crate) mod input;
pub(crate) mod input_event;
pub(crate) mod launcher;
pub(crate) mod mouse;
pub(crate) mod phase_timing;
pub(crate) mod presentation;
pub(crate) mod runner;
pub(crate) mod sessions;
pub(crate) mod setup;
pub(crate) mod slash;
pub(crate) mod surface;
pub(crate) mod surface_adapter;
pub(crate) mod timeline;
pub(crate) mod ui;
pub(crate) mod view_model;
pub(crate) mod workspace_git;
pub(crate) mod workspace_trust;

pub use app::AppState;
pub use appearance_diagnostics::appearance_doctor_checks;
pub use launcher::{
    install_current_boot_transaction, run_tui_resume_with_build_context, run_tui_with_build_context,
};
