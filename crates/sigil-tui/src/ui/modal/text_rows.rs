use ratatui::{
    layout::Alignment,
    style::Style,
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::ui::text::{sanitize_terminal_text, terminal_grapheme_width};

/// Modal-only wrapping. Count, scroll and draw these same safe visual rows.
pub(super) fn word_wrap_rows(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for line in lines {
        let mut builder = RowBuilder::new(width.max(1), line.style, line.alignment);
        let mut word = Vec::new();
        let mut ended_with_break = false;
        for span in line.spans {
            for grapheme in span.content.graphemes(true) {
                if grapheme.contains(['\n', '\r']) {
                    builder.append_word(&mut word);
                    builder.finish_row();
                    ended_with_break = true;
                    continue;
                }
                let Some(cells) = terminal_grapheme_width(grapheme) else {
                    continue;
                };
                let unit = (sanitize_terminal_text(grapheme), cells, span.style);
                if grapheme.chars().all(char::is_whitespace) {
                    builder.append_word(&mut word);
                    builder.append_grapheme(unit);
                } else {
                    word.push(unit);
                }
                ended_with_break = false;
            }
        }
        builder.append_word(&mut word);
        if !builder.spans.is_empty() || builder.rows.is_empty() || ended_with_break {
            builder.finish_row();
        }
        rows.extend(builder.rows);
    }
    if rows.is_empty() {
        rows.push(Line::default());
    }
    rows
}

struct RowBuilder {
    width: usize,
    used: usize,
    style: Style,
    alignment: Option<Alignment>,
    spans: Vec<Span<'static>>,
    rows: Vec<Line<'static>>,
}

impl RowBuilder {
    fn new(width: usize, style: Style, alignment: Option<Alignment>) -> Self {
        Self {
            width,
            used: 0,
            style,
            alignment,
            spans: Vec::new(),
            rows: Vec::new(),
        }
    }

    fn append_word(&mut self, word: &mut Vec<(String, usize, Style)>) {
        let cells = word.iter().map(|(_, cells, _)| *cells).sum::<usize>();
        if !word.is_empty() && self.used > 0 && self.used.saturating_add(cells) > self.width {
            self.finish_row();
        }
        for unit in word.drain(..) {
            self.append_grapheme(unit);
        }
    }

    fn append_grapheme(&mut self, (text, cells, style): (String, usize, Style)) {
        if self.used > 0 && self.used.saturating_add(cells) > self.width {
            self.finish_row();
        }
        if cells > self.width {
            self.spans.push(Span::styled("?", style));
            self.finish_row();
        } else {
            if let Some(previous) = self.spans.last_mut().filter(|span| span.style == style) {
                previous.content.to_mut().push_str(&text);
            } else {
                self.spans.push(Span::styled(text, style));
            }
            self.used += cells;
        }
    }

    fn finish_row(&mut self) {
        self.rows.push(Line {
            spans: std::mem::take(&mut self.spans),
            style: self.style,
            alignment: self.alignment,
        });
        self.used = 0;
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "../tests/modal_text_rows_tests.rs"]
mod tests;
