use ratatui::style::Style;

use super::ThemePalette;

pub(crate) fn body(palette: &ThemePalette) -> Style {
    Style::default()
        .fg(palette.text_primary)
        .bg(palette.surface_base)
}

pub(crate) fn muted(palette: &ThemePalette) -> Style {
    Style::default().fg(palette.text_muted)
}
