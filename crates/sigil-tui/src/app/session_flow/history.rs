use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

use sigil_kernel::{JsonlSessionStore, SessionLogEntry};

use super::super::{
    AppState, PaneFocus, SESSION_HISTORY_TITLE_SCAN_LIMIT, SessionHistoryEntry,
    formatting::truncate_session_view_text,
};

const SESSION_HISTORY_TITLE_LINE_MAX_BYTES: usize = 256 * 1024;

pub(super) fn session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    stem.strip_prefix("session-").map(ToOwned::to_owned)
}

pub(in crate::app) fn current_focus_label(app: &AppState) -> String {
    match app.active_pane {
        PaneFocus::Activity => format!("activity:{}", app.sidebar_selected_card.label()),
        other => other.label().to_owned(),
    }
}

pub(super) fn session_history_label(label: &str) -> String {
    label
        .strip_prefix("session-")
        .and_then(|value| value.strip_suffix(".jsonl"))
        .map(short_session_token)
        .unwrap_or_else(|| truncate_session_view_text(label, 24))
}

pub(in crate::app) fn session_history_display_label(entry: &SessionHistoryEntry) -> String {
    entry
        .title
        .as_deref()
        .map(|title| truncate_session_view_text(title, 48))
        .unwrap_or_else(|| session_history_label(&entry.label))
}

pub(super) fn session_history_title_from_log(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    for _ in 0..SESSION_HISTORY_TITLE_SCAN_LIMIT {
        let line = read_bounded_line(&mut reader, SESSION_HISTORY_TITLE_LINE_MAX_BYTES)
            .ok()
            .flatten()?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(Some(entry)) = JsonlSessionStore::session_entry_from_json_line(&line) else {
            continue;
        };
        if let SessionLogEntry::User(message) = entry
            && let Some(content) = message.content.as_deref().map(str::trim)
            && !content.is_empty()
        {
            return Some(truncate_session_view_text(content, 96));
        }
    }
    None
}

pub(super) fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut read_any = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !read_any {
                return Ok(None);
            }
            if too_long {
                return Ok(Some(String::new()));
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }

        read_any = true;
        let newline_index = available.iter().position(|byte| *byte == b'\n');
        let take_len = newline_index
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let chunk = &available[..take_len];
        if !too_long {
            if line.len() + chunk.len() <= max_bytes {
                line.extend_from_slice(chunk);
            } else {
                too_long = true;
                line.clear();
            }
        }
        reader.consume(take_len);

        if newline_index.is_some() {
            if too_long {
                return Ok(Some(String::new()));
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

pub(in crate::app) fn short_session_token(token: &str) -> String {
    token.chars().take(8).collect()
}
