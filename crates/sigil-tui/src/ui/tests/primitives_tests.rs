use ratatui::style::{Color, Modifier};

use super::*;
use crate::ui::theme::default_palette;

#[test]
fn timeline_header_line_styles_subtitle_with_muted_palette_color() {
    let palette = default_palette();
    let line = timeline_header_line_with_palette("Build", Color::Cyan, "agent: main", &palette);

    assert_eq!(line.spans.len(), 3);
    assert_eq!(line.spans[0].content.as_ref(), "Build");
    assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
    assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(line.spans[1].content.as_ref(), " ");
    assert_eq!(line.spans[2].content.as_ref(), "agent: main");
    assert_eq!(line.spans[2].style.fg, Some(palette.text_muted));
    assert!(line.spans[2].style.add_modifier.contains(Modifier::BOLD));
}
