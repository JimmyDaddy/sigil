#![cfg_attr(
    all(test, sigil_tui_test_slice_app_input_flow),
    allow(dead_code, unused_imports)
)]

pub mod app;
pub(crate) mod approval;
pub(crate) mod commands;
pub(crate) mod config_panel;
pub(crate) mod context_window;
pub(crate) mod input;
pub mod launcher;
pub mod mouse;
pub mod provider_status;
pub mod runner;
pub(crate) mod sessions;
pub(crate) mod setup;
pub(crate) mod slash;
pub(crate) mod timeline;
pub mod ui;
pub(crate) mod view_model;
