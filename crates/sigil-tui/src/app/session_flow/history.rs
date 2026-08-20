use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

use sigil_kernel::{ControlEntry, JsonlSessionStore, SessionLogEntry};

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
    for physical_line in 1..=SESSION_HISTORY_TITLE_SCAN_LIMIT {
        let line = read_bounded_line(&mut reader, SESSION_HISTORY_TITLE_LINE_MAX_BYTES)
            .ok()
            .flatten()?;
        if line.trim().is_empty() {
            continue;
        }
        let entry = match JsonlSessionStore::session_entry_from_json_line_at_path(
            &line,
            path,
            physical_line,
        ) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let Some(entry) = entry else {
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

pub(super) fn session_log_has_resumable_activity(path: &Path) -> bool {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    let mut reader = BufReader::new(file);
    for physical_line in 1.. {
        let line = match read_bounded_line(&mut reader, SESSION_HISTORY_TITLE_LINE_MAX_BYTES) {
            Ok(Some(line)) => line,
            Ok(None) => return false,
            Err(_) => return true,
        };
        if line.is_empty() {
            // `read_bounded_line` uses an empty string to report an oversized physical line.
            // Oversized input is not proof of an empty session, so keep it fail-safe visible.
            return true;
        }
        if line.trim().is_empty() {
            continue;
        }
        match JsonlSessionStore::session_entry_from_json_line_at_path(&line, path, physical_line) {
            Ok(Some(entry)) if session_entry_has_resumable_activity(&entry) => return true,
            Ok(Some(_)) => {}
            // Direct durable records and invalid streams are not bootstrap-only. Keep them
            // visible so this lightweight product filter can never hide recoverable state.
            Ok(None) | Err(_) => return true,
        }
    }
    unreachable!("unbounded physical-line iterator must end through EOF")
}

pub(super) fn session_entries_have_resumable_activity(entries: &[SessionLogEntry]) -> bool {
    entries.iter().any(session_entry_has_resumable_activity)
}

pub(super) fn discard_bootstrap_only_session_file(
    path: &Path,
    session_log_dir: &Path,
) -> std::io::Result<bool> {
    if session_log_has_resumable_activity(path) {
        return Ok(false);
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(false);
    }
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return Ok(false);
    };
    if !file_name.starts_with("session-")
        || path.extension().and_then(|value| value.to_str()) != Some("jsonl")
    {
        return Ok(false);
    }
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    if fs::canonicalize(parent)? != fs::canonicalize(session_log_dir)? {
        return Ok(false);
    }

    let mut resource_path = path.to_path_buf();
    resource_path.set_extension("");
    if resource_path.exists() {
        return Ok(false);
    }

    fs::remove_file(path)?;
    for suffix in [".writer-lock", ".attachment-lock", ".attachment-generation"] {
        let sidecar = parent.join(format!("{file_name}{suffix}"));
        let Ok(metadata) = fs::symlink_metadata(&sidecar) else {
            continue;
        };
        if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
            let _ = fs::remove_file(sidecar);
        }
    }
    Ok(true)
}

fn session_entry_has_resumable_activity(entry: &SessionLogEntry) -> bool {
    !matches!(
        entry,
        SessionLogEntry::Control(
            ControlEntry::SessionIdentity { .. }
                | ControlEntry::SessionModelSelected { .. }
                | ControlEntry::SessionRouteRebound { .. }
                | ControlEntry::SessionRouteTrustBound { .. }
                | ControlEntry::WorkspaceTrustDecision(_)
        )
    )
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
