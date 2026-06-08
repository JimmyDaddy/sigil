mod approval;
mod composer;
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
// Compatibility re-export for callers that do not need custom timeline options.
#[allow(unused_imports)]
pub(crate) use timeline::render_timeline_entry_lines;
