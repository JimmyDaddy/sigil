mod approval;
mod checkpoint_restore;
mod command_text;
mod composer;
mod diff;
mod egress_disclosure;
mod geometry;
mod info_rail;
mod intent_stack;
mod layout_snapshot;
mod live_panel;
mod markdown;
mod modal;
mod plan_workbench;
mod primitives;
mod setup_config;
mod shell;
mod slash_overlay;
mod status_indicator;
mod syntax_highlight;
mod text;
pub(crate) mod theme;
mod timeline;
mod tool_card;
mod user_input_form;

#[cfg(test)]
pub use shell::render;
pub(crate) use shell::render_surface;

pub(crate) use layout_snapshot::live_transcript_rows_for_app;
pub use layout_snapshot::{LayoutMode, LayoutSnapshot};

pub(crate) use markdown::MarkdownRenderCursor;
pub(crate) use markdown::contains_mermaid_diagram as markdown_contains_mermaid_diagram;
pub(crate) use text::{
    bidi_reorder_line, sanitize_terminal_text, terminal_cell_width, terminal_grapheme_width,
    visual_position_for_char_cursor, wrap_terminal_lines,
};
pub(crate) use timeline::{
    TimelineRenderOptions, render_timeline_entry_lines_with_options,
    render_timeline_entry_lines_with_options_and_cursor, thinking_has_collapsed_content,
};
pub(crate) use tool_card::tool_activity_view;

pub(crate) use checkpoint_restore::checkpoint_restore_max_scroll;
pub(crate) use status_indicator::{
    FocusKind, StatusKind, focus_symbol, status_kind_from_label, status_symbol,
};
