#![cfg_attr(
    all(test, sigil_tui_test_slice_app_input_flow),
    allow(dead_code, unused_imports)
)]

#[cfg(test)]
#[macro_use]
#[path = "tests/mcp_config_macros.rs"]
mod mcp_config_macros;

pub(crate) mod agent_display;
pub mod app;
pub mod appearance_diagnostics;
pub(crate) mod approval;
pub(crate) mod commands;
pub(crate) mod config_panel;
pub(crate) mod input;
pub mod launcher;
pub mod mouse;
pub mod runner;
pub(crate) mod sessions;
pub(crate) mod setup;
pub(crate) mod slash;
pub(crate) mod timeline;
pub mod ui;
pub(crate) mod view_model;
pub(crate) mod workspace_trust;
