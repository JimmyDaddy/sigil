mod approval;
mod composer;
mod diff;
mod geometry;
mod info_rail;
mod live_panel;
mod markdown;
mod modal;
mod primitives;
mod setup_config;
mod shell;
mod slash_overlay;
mod text;
mod theme;
mod timeline;
mod tool_card;

pub use shell::render;

pub(crate) use timeline::{TimelineRenderOptions, render_timeline_entry_lines_with_options};
pub(crate) use tool_card::{render_inspected_group_lines, tool_activity_view};
