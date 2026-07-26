#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkdownRepairDiagnostic {
    pub(super) kind: MarkdownRepairDiagnosticKind,
    pub(super) source_start: usize,
    pub(super) source_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkdownRepairDiagnosticKind {
    AttachedClosingFence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NormalizedMarkdown {
    pub(super) source: String,
    pub(super) diagnostics: Vec<MarkdownRepairDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fence {
    pub(super) marker: char,
    pub(super) length: usize,
    indent: usize,
}

pub(super) fn normalize_completed_markdown(source: &str) -> NormalizedMarkdown {
    let mut normalized = Vec::new();
    let mut diagnostics = Vec::new();
    let mut fence = None;
    let mut source_offset = 0usize;

    for line in source.split('\n') {
        let Some(active_fence) = fence else {
            fence = opening_fence(line);
            normalized.push(line.to_owned());
            source_offset = source_offset.saturating_add(line.len()).saturating_add(1);
            continue;
        };

        if is_closing_fence(line, active_fence) {
            fence = None;
            normalized.push(line.to_owned());
            source_offset = source_offset.saturating_add(line.len()).saturating_add(1);
            continue;
        }

        let Some((content, closing)) = split_attached_closing_fence(line, active_fence) else {
            normalized.push(line.to_owned());
            source_offset = source_offset.saturating_add(line.len()).saturating_add(1);
            continue;
        };

        let marker_start = source_offset.saturating_add(content.len());
        let marker_len = closing.trim_start().len();
        diagnostics.push(MarkdownRepairDiagnostic {
            kind: MarkdownRepairDiagnosticKind::AttachedClosingFence,
            source_start: marker_start,
            source_end: marker_start.saturating_add(marker_len),
        });
        normalized.push(content);
        normalized.push(closing);
        source_offset = source_offset.saturating_add(line.len()).saturating_add(1);
        fence = None;
    }

    NormalizedMarkdown {
        source: normalized.join("\n"),
        diagnostics,
    }
}

pub(super) fn normalize_streaming_markdown(source: &str) -> NormalizedMarkdown {
    NormalizedMarkdown {
        source: source.to_owned(),
        diagnostics: Vec::new(),
    }
}

pub(super) fn opening_fence(line: &str) -> Option<Fence> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let marker = rest.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let length = rest.chars().take_while(|value| *value == marker).count();
    if length < 3 {
        return None;
    }
    let marker_bytes = marker.len_utf8().saturating_mul(length);
    if marker == '`' && rest[marker_bytes..].contains('`') {
        return None;
    }
    Some(Fence {
        marker,
        length,
        indent,
    })
}

pub(super) fn is_closing_fence(line: &str, fence: Fence) -> bool {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    let trimmed = line.trim();
    indent <= 3
        && trimmed.chars().count() >= fence.length
        && trimmed.chars().all(|character| character == fence.marker)
}

fn split_attached_closing_fence(line: &str, fence: Fence) -> Option<(String, String)> {
    let trimmed_end = line.trim_end();
    let marker_byte = fence.marker as u8;
    let bytes = trimmed_end.as_bytes();
    let mut marker_start = bytes.len();
    while marker_start > 0 && bytes[marker_start - 1] == marker_byte {
        marker_start -= 1;
    }
    if bytes.len().saturating_sub(marker_start) < fence.length
        || trimmed_end[..marker_start].trim().is_empty()
    {
        return None;
    }
    Some((
        trimmed_end[..marker_start].trim_end().to_owned(),
        format!(
            "{}{}",
            " ".repeat(fence.indent),
            &trimmed_end[marker_start..]
        ),
    ))
}
