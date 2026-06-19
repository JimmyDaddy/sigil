mod approval;
mod composer;
mod diff;
mod geometry;
mod info_rail;
mod layout_snapshot;
mod live_panel;
mod markdown;
mod modal;
mod primitives;
mod setup_config;
mod shell;
mod slash_overlay;
mod status_indicator;
mod syntax_highlight;
mod text;
mod theme;
mod timeline;
mod tool_card;

pub use shell::render;

pub use layout_snapshot::{LayoutMode, LayoutSnapshot};

pub(crate) use timeline::{TimelineRenderOptions, render_timeline_entry_lines_with_options};
pub(crate) use tool_card::tool_activity_view;

pub(crate) use status_indicator::{
    FocusKind, StatusKind, focus_symbol, status_kind_from_label, status_symbol,
};
