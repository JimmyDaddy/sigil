use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn truncate_inline_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

pub(crate) fn truncate_display_width(text: &str, max_width: usize) -> String {
    let max_width = max_width.max(1);
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    let ellipsis = "...";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    let budget = max_width.saturating_sub(ellipsis_width).max(1);
    let mut out = String::new();
    let mut used_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if !out.is_empty() && used_width + grapheme_width > budget {
            break;
        }
        out.push_str(grapheme);
        used_width += grapheme_width;
    }
    format!("{out}{ellipsis}")
}

pub(crate) fn wrap_display_width(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if !current.is_empty() && current_width + grapheme_width > width {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    if current.is_empty() {
        rows.push(String::new());
    } else {
        rows.push(current);
    }
    rows
}

pub(crate) fn wrap_composer_input(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        rows.extend(wrap_display_width(line, width));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

pub(crate) fn pad_display_width(text: &str, width: usize) -> String {
    let mut out = text.to_owned();
    let display_width = UnicodeWidthStr::width(text);
    if width > display_width {
        out.push_str(&" ".repeat(width - display_width));
    }
    out
}

pub(crate) fn wrapped_line_rows(line: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let display_width = UnicodeWidthStr::width(line);
    if display_width == 0 {
        return 1;
    }
    display_width.div_ceil(width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_line_rows_counts_visual_rows() {
        assert_eq!(wrapped_line_rows("", 10), 1);
        assert_eq!(wrapped_line_rows("short", 10), 1);
        assert_eq!(wrapped_line_rows("1234567890", 10), 1);
        assert_eq!(wrapped_line_rows("12345678901", 10), 2);
        assert_eq!(wrapped_line_rows("你好", 2), 2);
    }
}
