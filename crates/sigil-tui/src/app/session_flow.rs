use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sigil_kernel::{
    CompactionPreview, ControlEntry, JsonlSessionStore, ModelMessage, RootConfig, Session,
    SessionLogEntry, ToolEgressEntry, ToolExecutionEntry, ToolExecutionStatus, ToolPreviewSnapshot,
    inspect_memory_documents, latest_compaction_record, session_stats_from_entries,
};
use uuid::Uuid;

use super::{
    AppState, PaneFocus, RunPhase, SESSION_HISTORY_TITLE_SCAN_LIMIT, SessionHistoryEntry,
    SessionHistoryRow, SessionViewMode, TimelineRole,
    formatting::{
        format_terminal_task_block_redacted, format_tool_content_block_redacted_for_restore,
        human_file_size, relative_age_label, truncate_session_view_text,
    },
};

const SESSION_HISTORY_TITLE_LINE_MAX_BYTES: usize = 256 * 1024;

impl AppState {
    pub fn restore_latest_session_from_disk(&mut self, root_config: &RootConfig) -> bool {
        self.refresh_session_history();
        let Some(session_log_path) = self.session_history.first().map(|entry| entry.path.clone())
        else {
            return false;
        };

        self.restore_session_path_from_disk(
            session_log_path,
            &root_config.agent.provider,
            &root_config.agent.model,
            "restored latest session",
        )
    }

    pub fn restore_session_path_from_disk(
        &mut self,
        session_log_path: PathBuf,
        fallback_provider_name: &str,
        fallback_model_name: &str,
        notice: &str,
    ) -> bool {
        let Ok(store) = JsonlSessionStore::new(&session_log_path) else {
            return false;
        };
        let Ok(session) = Session::load_from_store(
            fallback_provider_name.to_owned(),
            fallback_model_name.to_owned(),
            store,
        ) else {
            return false;
        };

        let provider_name = session.provider_name().to_owned();
        let model_name = session.model_name().to_owned();
        let entries = session.entries().to_vec();
        self.restore_session_view(session_log_path, provider_name, model_name, entries, notice);
        self.last_notice = Some(notice.to_owned());
        self.refresh_session_history();
        true
    }

    pub(super) fn session_view_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("{} view", self.session_view_mode.label()),
            format!(
                "compact={}  prompt={}  cache={:.0}%",
                self.compaction_status,
                self.stats.last_prompt_tokens,
                self.cache_hit_ratio() * 100.0
            ),
        ];
        if self.is_busy {
            lines.push("running; durable view".to_owned());
        }

        lines.push(String::new());
        lines.extend(match self.session_view_mode {
            SessionViewMode::Provider => self.provider_projection_lines(),
            SessionViewMode::Audit => self.audit_log_lines(),
        });
        lines.push(String::new());
        lines.extend(self.recent_session_lines());
        lines.push(String::new());
        lines.push("V view  type filter  Enter/1-9 resume".to_owned());
        lines.push("Backspace edit  Esc clear  Arrows/Pg move".to_owned());
        lines
    }

    fn provider_projection_lines(&self) -> Vec<String> {
        if self.current_session_entries.is_empty() {
            return vec!["no provider messages".to_owned()];
        }

        let session = Session::from_entries(
            self.provider_name.clone(),
            self.model_name.clone(),
            self.current_session_entries.clone(),
        );
        let messages = session.messages();
        let mut lines = vec!["Provider:".to_owned()];
        if let Some(record) = &self.latest_compaction_record {
            lines.push(format!(
                "  summary: compacted={} tail={}",
                record.compacted_message_count, record.retained_tail_message_count
            ));
        }
        for message in messages {
            lines.push(render_model_message_line(&message));
        }
        if !self.is_busy {
            match session.compaction_preview(&self.compaction_config) {
                Ok(Some(preview)) => {
                    lines.push(String::new());
                    lines.extend(render_compaction_preview_lines(&preview));
                }
                Ok(None) => {
                    lines.push(String::new());
                    lines.push("/compact preview: nothing to fold".to_owned());
                }
                Err(error) => {
                    lines.push(String::new());
                    lines.push(format!("/compact preview unavailable: {error}"));
                }
            }
        }
        lines
    }

    fn audit_log_lines(&self) -> Vec<String> {
        if self.current_session_entries.is_empty() {
            return vec!["no audit entries".to_owned()];
        }

        let mut lines = vec!["Audit:".to_owned()];
        for entry in &self.current_session_entries {
            lines.push(render_session_log_entry(entry));
        }
        lines
    }

    pub(super) fn recent_session_lines(&self) -> Vec<String> {
        self.recent_session_rows()
            .into_iter()
            .map(|row| match row {
                SessionHistoryRow::SessionHeader { filter, total } => {
                    format!("filter={filter} total={total}")
                }
                SessionHistoryRow::SessionItem {
                    index,
                    label,
                    current,
                    selected,
                    meta,
                } => format!(
                    "{} {}. {}{} {}",
                    if selected { ">" } else { " " },
                    index,
                    label,
                    if current { " (current)" } else { "" },
                    meta
                ),
                SessionHistoryRow::Empty { text } => text,
            })
            .collect()
    }

    pub(super) fn recent_session_rows(&self) -> Vec<SessionHistoryRow> {
        let filtered_indices = self.filtered_session_indices();
        let mut rows = vec![SessionHistoryRow::SessionHeader {
            filter: if self.session_history_filter.is_empty() {
                "-".to_owned()
            } else {
                self.session_history_filter.clone()
            },
            total: filtered_indices.len(),
        }];
        if filtered_indices.is_empty() {
            rows.push(SessionHistoryRow::Empty {
                text: "no matches".to_owned(),
            });
            return rows;
        }

        let start = self
            .session_history_selected
            .saturating_sub(self.session_history_visible_limit / 2)
            .min(filtered_indices.len().saturating_sub(1));
        let end = (start + self.session_history_visible_limit).min(filtered_indices.len());
        for (filtered_index, entry_index) in filtered_indices
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            let entry = &self.session_history[*entry_index];
            rows.push(SessionHistoryRow::SessionItem {
                index: filtered_index + 1,
                label: session_history_display_label(entry),
                current: entry.path == self.session_log_path,
                selected: filtered_index == self.session_history_selected,
                meta: format!(
                    "{} · {}",
                    human_file_size(entry.bytes),
                    relative_age_label(entry.modified_epoch_secs)
                ),
            });
        }
        rows
    }

    pub(super) fn sync_current_session_state(&mut self, entries: Vec<SessionLogEntry>) {
        self.stats = session_stats_from_entries(&entries);
        self.latest_compaction_record = latest_compaction_record(&entries);
        self.tool_preview_snapshots = restored_tool_preview_snapshot_index(&entries);
        self.current_session_entries = entries;
        self.refresh_active_agent_view_after_parent_sync();
        self.refresh_usage_sidebar_cache();
    }

    pub(super) fn append_current_session_control(&mut self, control: ControlEntry) {
        self.current_session_entries
            .push(SessionLogEntry::Control(control));
        self.stats = session_stats_from_entries(&self.current_session_entries);
        self.latest_compaction_record = latest_compaction_record(&self.current_session_entries);
        self.tool_preview_snapshots =
            restored_tool_preview_snapshot_index(&self.current_session_entries);
        self.refresh_active_agent_view_after_parent_sync();
        self.refresh_usage_sidebar_cache();
    }

    pub(super) fn refresh_session_history(&mut self) {
        let mut sessions = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.session_log_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_jsonl = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.eq_ignore_ascii_case("jsonl"))
                    .unwrap_or(false);
                if !is_jsonl {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let label = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("unknown")
                    .to_owned();
                let modified_epoch_secs = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                let bytes = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                let title = session_history_title_from_log(&path);
                sessions.push((
                    modified,
                    SessionHistoryEntry {
                        path,
                        label,
                        title,
                        modified_epoch_secs,
                        bytes,
                    },
                ));
            }
        }
        sessions.sort_by(|left, right| right.0.cmp(&left.0));
        self.session_history = sessions.into_iter().map(|(_, entry)| entry).collect();
        let current_index = self
            .session_history
            .iter()
            .position(|entry| entry.path == self.session_log_path)
            .unwrap_or(0);
        self.session_history_selected = self
            .filtered_session_indices()
            .iter()
            .position(|index| *index == current_index)
            .unwrap_or(0)
            .min(self.filtered_session_indices().len().saturating_sub(1));
    }

    pub(super) fn refresh_memory_summary(&mut self) {
        match inspect_memory_documents(&self.workspace_root, &self.memory_config) {
            Ok(report) => {
                self.memory_enabled = report.enabled;
                self.memory_document_count = report.document_count;
                self.memory_last_status = "ok".to_owned();
            }
            Err(error) => {
                self.memory_enabled = self.memory_config.enabled;
                self.memory_document_count = 0;
                self.memory_last_status = error.to_string();
            }
        }
    }

    pub(super) fn resolve_resume_target(&self, selector: &str) -> Option<PathBuf> {
        if self.session_history.is_empty() {
            return None;
        }

        let normalized = if selector.is_empty() {
            "latest"
        } else {
            selector
        };
        let candidate_indices = self.resume_candidate_indices();
        if normalized.eq_ignore_ascii_case("latest") {
            return candidate_indices
                .first()
                .and_then(|index| self.session_history.get(*index))
                .map(|entry| entry.path.clone());
        }

        if let Some(path) = normalized
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| candidate_indices.get(index).copied())
            .and_then(|index| self.session_history.get(index))
            .map(|entry| entry.path.clone())
        {
            return Some(path);
        }

        let path = PathBuf::from(normalized);
        if self.session_history.iter().any(|entry| entry.path == path) {
            return Some(path);
        }

        let query = normalized.to_ascii_lowercase();
        let mut matches = candidate_indices
            .into_iter()
            .filter_map(|index| self.session_history.get(index))
            .filter(|entry| {
                entry.label.to_ascii_lowercase().contains(&query)
                    || entry
                        .title
                        .as_deref()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains(&query)
                    || entry
                        .path
                        .display()
                        .to_string()
                        .to_ascii_lowercase()
                        .contains(&query)
            });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(first.path.clone())
    }

    pub(super) fn resume_candidate_indices(&self) -> Vec<usize> {
        let non_current = self
            .session_history
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| (entry.path != self.session_log_path).then_some(index))
            .collect::<Vec<_>>();
        if non_current.is_empty() {
            return (0..self.session_history.len()).collect();
        }
        non_current
    }

    pub(super) fn restore_session_view(
        &mut self,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
        notice: &str,
    ) {
        self.session_log_path = session_log_path;
        self.provider_name = provider_name;
        self.model_name = model_name;
        self.session_id = session_id_from_path(&self.session_log_path)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.active_agent_view = super::AgentView::Main;
        self.active_agent_child_transcript = None;
        self.sync_current_session_state(entries.clone());
        self.pending_approval = None;
        self.run_phase = RunPhase::Idle;
        self.refresh_memory_summary();
        self.recompute_compaction_status(false);
        self.timeline.clear();
        self.tool_activity_cache.clear();
        self.expanded_thinking_entry_indices.clear();
        self.collapsed_thinking_entry_indices.clear();
        self.events.clear();
        self.reset_scroll();

        self.push_timeline(
            TimelineRole::System,
            format!("Resumed {}.", self.session_id),
        );
        self.push_timeline(TimelineRole::Notice, notice.to_owned());
        self.push_event("session", format!("active {}", self.session_id));
        self.push_event("workspace", self.workspace_root.display().to_string());
        self.push_event(
            "model",
            format!("{}/{}", self.provider_name, self.model_name),
        );
        self.push_event("effort", self.reasoning_effort.as_str());
        self.push_event("approval_default", self.permission_default_mode.clone());
        self.push_event(
            "memory",
            format!(
                "enabled={} docs={} status={}",
                self.memory_enabled, self.memory_document_count, self.memory_last_status
            ),
        );
        self.push_event("compaction", self.compaction_status.clone());
        self.push_event("session_log", self.session_log_path.display().to_string());
        self.push_event("focus", self.active_pane.label());
        self.push_event("restore", format!("entries={}", entries.len()));

        let restored_tool_executions = restored_tool_execution_index(&entries);
        let restored_tool_previews = restored_tool_preview_snapshot_index(&entries);
        let restored_tool_result_call_ids = restored_tool_result_call_ids(&entries);
        self.tool_preview_snapshots = restored_tool_previews.clone();
        for entry in entries {
            match entry {
                SessionLogEntry::User(message) => {
                    if let Some(content) = message.content {
                        self.push_timeline(TimelineRole::User, content);
                    }
                }
                SessionLogEntry::Assistant(message) => {
                    if let Some(content) = message.content
                        && !content.is_empty()
                    {
                        self.push_timeline(TimelineRole::Assistant, content);
                    }
                }
                SessionLogEntry::ToolResult(message) => {
                    if let Some(content) = message.content {
                        let execution = message
                            .tool_call_id
                            .as_deref()
                            .and_then(|call_id| restored_tool_executions.get(call_id));
                        let preview = message
                            .tool_call_id
                            .as_deref()
                            .and_then(|call_id| restored_tool_previews.get(call_id));
                        self.push_timeline(
                            TimelineRole::Tool,
                            format_tool_content_block_redacted_for_restore(
                                message.tool_call_id.as_deref(),
                                &content,
                                execution,
                                preview,
                                &self.secret_redactor,
                            ),
                        );
                    }
                }
                SessionLogEntry::Control(control) => match control {
                    ControlEntry::Note { kind, data }
                        if kind == "reasoning_delta" || kind == "reasoning_trace" =>
                    {
                        if let Some(delta) = restored_reasoning_note(&kind, &data) {
                            self.push_restored_reasoning_delta(&delta);
                        }
                    }
                    ControlEntry::ToolExecution(execution)
                        if should_render_restored_tool_execution(
                            execution.as_ref(),
                            &restored_tool_result_call_ids,
                        ) =>
                    {
                        let preview = restored_tool_previews.get(&execution.call_id);
                        self.push_timeline(
                            TimelineRole::Tool,
                            format_tool_content_block_redacted_for_restore(
                                Some(execution.call_id.as_str()),
                                &restored_tool_execution_content(execution.as_ref()),
                                Some(execution.as_ref()),
                                preview,
                                &self.secret_redactor,
                            ),
                        );
                        self.push_event("control:restore", format!("{execution:?}"));
                    }
                    ControlEntry::TerminalTask(task) => {
                        self.push_timeline(
                            TimelineRole::Tool,
                            format_terminal_task_block_redacted(&task, &self.secret_redactor),
                        );
                        self.push_event(
                            "control:restore",
                            format!(
                                "terminal {} status={}",
                                task.handle.task_id.as_str(),
                                task.status.as_str()
                            ),
                        );
                    }
                    other => {
                        self.push_event("control:restore", format!("{other:?}"));
                    }
                },
            }
        }

        self.streaming_reasoning_index = None;
        self.last_notice = Some(notice.to_owned());
        self.refresh_session_history();
    }

    fn push_restored_reasoning_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(index) = self
            .timeline
            .len()
            .checked_sub(1)
            .filter(|index| self.timeline[*index].role == TimelineRole::Thinking)
        {
            self.timeline[index].text.push_str(delta);
            self.rerender_timeline_entry(index);
            return;
        }
        self.push_phase_marker(format!("thinking|{}", self.model_name));
        self.push_timeline(TimelineRole::Thinking, delta.to_owned());
    }

    pub(super) fn restored_timeline_entries_from_session_entries(
        &self,
        entries: &[SessionLogEntry],
    ) -> Vec<crate::timeline::TimelineEntry> {
        restored_timeline_entries_from_session_entries(entries, &self.secret_redactor)
    }

    pub(super) fn filtered_session_indices(&self) -> Vec<usize> {
        let filter = self.session_history_filter.to_ascii_lowercase();
        self.session_history
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let include =
                    filter.is_empty() || entry.label.to_ascii_lowercase().contains(&filter);
                include.then_some(index)
            })
            .collect()
    }
}

fn restored_timeline_entries_from_session_entries(
    entries: &[SessionLogEntry],
    redactor: &sigil_kernel::SecretRedactor,
) -> Vec<crate::timeline::TimelineEntry> {
    let restored_tool_executions = restored_tool_execution_index(entries);
    let restored_tool_previews = restored_tool_preview_snapshot_index(entries);
    let restored_tool_result_call_ids = restored_tool_result_call_ids(entries);
    let mut timeline = Vec::new();
    for entry in entries {
        match entry {
            SessionLogEntry::User(message) => {
                if let Some(content) = message.content.as_ref() {
                    timeline.push(crate::timeline::TimelineEntry {
                        role: TimelineRole::User,
                        text: content.clone(),
                    });
                }
            }
            SessionLogEntry::Assistant(message) => {
                if let Some(content) = message.content.as_ref()
                    && !content.is_empty()
                {
                    timeline.push(crate::timeline::TimelineEntry {
                        role: TimelineRole::Assistant,
                        text: content.clone(),
                    });
                }
            }
            SessionLogEntry::ToolResult(message) => {
                if let Some(content) = message.content.as_ref() {
                    let execution = message
                        .tool_call_id
                        .as_deref()
                        .and_then(|call_id| restored_tool_executions.get(call_id));
                    let preview = message
                        .tool_call_id
                        .as_deref()
                        .and_then(|call_id| restored_tool_previews.get(call_id));
                    timeline.push(crate::timeline::TimelineEntry {
                        role: TimelineRole::Tool,
                        text: format_tool_content_block_redacted_for_restore(
                            message.tool_call_id.as_deref(),
                            content,
                            execution,
                            preview,
                            redactor,
                        ),
                    });
                }
            }
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if kind == "reasoning_delta" || kind == "reasoning_trace" =>
            {
                if let Some(delta) = restored_reasoning_note(kind, data) {
                    push_restored_reasoning_timeline_entry(&mut timeline, &delta);
                }
            }
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if should_render_restored_tool_execution(
                    execution.as_ref(),
                    &restored_tool_result_call_ids,
                ) =>
            {
                let preview = restored_tool_previews.get(&execution.call_id);
                timeline.push(crate::timeline::TimelineEntry {
                    role: TimelineRole::Tool,
                    text: format_tool_content_block_redacted_for_restore(
                        Some(execution.call_id.as_str()),
                        &restored_tool_execution_content(execution.as_ref()),
                        Some(execution.as_ref()),
                        preview,
                        redactor,
                    ),
                });
            }
            SessionLogEntry::Control(ControlEntry::TerminalTask(task)) => {
                timeline.push(crate::timeline::TimelineEntry {
                    role: TimelineRole::Tool,
                    text: format_terminal_task_block_redacted(task, redactor),
                });
            }
            SessionLogEntry::Control(_) => {}
        }
    }
    timeline
}

fn push_restored_reasoning_timeline_entry(
    timeline: &mut Vec<crate::timeline::TimelineEntry>,
    delta: &str,
) {
    if delta.is_empty() {
        return;
    }
    if let Some(entry) = timeline
        .last_mut()
        .filter(|entry| entry.role == TimelineRole::Thinking)
    {
        entry.text.push_str(delta);
        return;
    }
    timeline.push(crate::timeline::TimelineEntry {
        role: TimelineRole::Thinking,
        text: delta.to_owned(),
    });
}

fn session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    stem.strip_prefix("session-").map(ToOwned::to_owned)
}

pub(super) fn current_focus_label(app: &AppState) -> String {
    match app.active_pane {
        PaneFocus::Activity => format!("activity:{}", app.sidebar_selected_card.label()),
        other => other.label().to_owned(),
    }
}

fn session_history_label(label: &str) -> String {
    label
        .strip_prefix("session-")
        .and_then(|value| value.strip_suffix(".jsonl"))
        .map(short_session_token)
        .unwrap_or_else(|| truncate_session_view_text(label, 24))
}

pub(super) fn session_history_display_label(entry: &SessionHistoryEntry) -> String {
    entry
        .title
        .as_deref()
        .map(|title| truncate_session_view_text(title, 48))
        .unwrap_or_else(|| session_history_label(&entry.label))
}

fn session_history_title_from_log(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    for _ in 0..SESSION_HISTORY_TITLE_SCAN_LIMIT {
        let line = read_bounded_line(&mut reader, SESSION_HISTORY_TITLE_LINE_MAX_BYTES)
            .ok()
            .flatten()?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<SessionLogEntry>(&line) else {
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

fn read_bounded_line<R: BufRead>(
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

pub(super) fn short_session_token(token: &str) -> String {
    token.chars().take(8).collect()
}

fn render_model_message_line(message: &ModelMessage) -> String {
    let role = match message.role {
        sigil_kernel::MessageRole::System => "system",
        sigil_kernel::MessageRole::User => "user",
        sigil_kernel::MessageRole::Assistant => "assistant",
        sigil_kernel::MessageRole::Tool => "tool",
    };
    if !message.tool_calls.is_empty() {
        let names = message
            .tool_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let content =
            truncate_session_view_text(message.content.as_deref().unwrap_or_default(), 160);
        if !content.is_empty() {
            return format!("[{role}] {content} tool_calls [{names}]");
        }
        return format!("[{role}] tool_calls [{names}]");
    }

    let content = truncate_session_view_text(message.content.as_deref().unwrap_or_default(), 160);
    if matches!(message.role, sigil_kernel::MessageRole::Tool) {
        format!(
            "[{role}] {} => {content}",
            message.tool_call_id.as_deref().unwrap_or("unknown")
        )
    } else {
        format!("[{role}] {content}")
    }
}

fn render_session_log_entry(entry: &SessionLogEntry) -> String {
    match entry {
        SessionLogEntry::User(message)
        | SessionLogEntry::Assistant(message)
        | SessionLogEntry::ToolResult(message) => render_model_message_line(message),
        SessionLogEntry::Control(control) => match control {
            ControlEntry::SessionIdentity {
                provider_name,
                model_name,
            } => format!("[ctl] session {provider_name}/{model_name}"),
            ControlEntry::ContinuationStateSaved(state) => format!(
                "[ctl] cont {} msg={}",
                state.state_kind,
                state.message_id.as_deref().unwrap_or("-")
            ),
            ControlEntry::ResponseHandleTracked(handle) => format!(
                "[ctl] response {}",
                truncate_session_view_text(&handle.response_id, 48)
            ),
            ControlEntry::BackgroundTaskTracked(handle) => format!("[ctl] task {}", handle.task_id),
            ControlEntry::PrefixSnapshotCaptured(snapshot) => format!(
                "[ctl] prefix sha={} mem={}",
                truncate_session_view_text(&snapshot.sha256, 16),
                truncate_session_view_text(&snapshot.memory_fingerprint, 16)
            ),
            ControlEntry::MemorySnapshotCaptured(snapshot) => format!(
                "[ctl] memory docs={} fp={}",
                snapshot.report.document_count,
                truncate_session_view_text(&snapshot.report.fingerprint, 16)
            ),
            ControlEntry::UsageSnapshot(usage) => format!(
                "[ctl] usage p={} c={} hit={} miss={}",
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.cache_hit_tokens,
                usage.cache_miss_tokens
            ),
            ControlEntry::ToolApproval(approval) => format!(
                "[ctl] approval {} {} action={} mode={}",
                approval.call_id,
                approval.tool_name,
                tool_approval_action_label(approval.action),
                approval.policy_decision.as_str()
            ),
            ControlEntry::ToolExecution(execution) => format!(
                "[ctl] execution {} {} status={}",
                execution.call_id,
                execution.tool_name,
                tool_execution_status_label(execution.status)
            ),
            ControlEntry::ToolEgress(egress) => render_tool_egress_line(egress),
            ControlEntry::McpElicitation(elicitation) => format!(
                "[ctl] mcp elicitation {} action={} fields={}",
                truncate_session_view_text(&elicitation.server_name, 48),
                mcp_elicitation_decision_label(elicitation.action),
                elicitation.requested_field_names.len()
            ),
            ControlEntry::ToolPreviewCaptured(snapshot) => format!(
                "[ctl] preview {} {} files={} +{} -{}",
                snapshot.call_id,
                snapshot.tool_name,
                snapshot.file_diffs.len(),
                snapshot.original_stats.added,
                snapshot.original_stats.removed
            ),
            ControlEntry::SkillIndexCaptured(snapshot) => format!(
                "[ctl] skills index count={} fp={}",
                snapshot.descriptors.len(),
                truncate_session_view_text(&snapshot.fingerprint, 16)
            ),
            ControlEntry::SkillLoaded(entry) => format!(
                "[ctl] skill {} loaded bytes={} lines={}",
                truncate_session_view_text(&entry.skill_id, 48),
                entry.byte_count,
                entry.line_count
            ),
            ControlEntry::PluginManifestCaptured(snapshot) => format!(
                "[ctl] plugin {} version={} caps={} trust={}",
                truncate_session_view_text(&snapshot.plugin_id, 48),
                truncate_session_view_text(&snapshot.version, 24),
                snapshot.capabilities.len(),
                snapshot.trust.as_str()
            ),
            ControlEntry::PluginTrustDecision(entry) => format!(
                "[ctl] plugin {} trust={} hash={}",
                truncate_session_view_text(&entry.plugin_id, 48),
                entry.decision.as_str(),
                truncate_session_view_text(&entry.manifest_hash, 16)
            ),
            ControlEntry::ChangeSetProposed(change_set) => format!(
                "[ctl] changeset {} proposed risk={} files={} {}",
                change_set.id.as_str(),
                change_set.risk.as_str(),
                change_set.files.len(),
                truncate_session_view_text(&change_set.title, 48)
            ),
            ControlEntry::ChangeSetApplied(result) => format!(
                "[ctl] changeset {} status={} files={}",
                result.id.as_str(),
                result.status.as_str(),
                result.file_results.len()
            ),
            ControlEntry::TerminalTask(task) => format!(
                "[ctl] terminal {} status={} log={}",
                task.handle.task_id.as_str(),
                task.status.as_str(),
                truncate_session_view_text(&task.handle.log_path.display().to_string(), 48)
            ),
            ControlEntry::CompactionApplied(record) => format!(
                "[ctl] compacted={} tail={}",
                record.compacted_message_count, record.retained_tail_message_count
            ),
            ControlEntry::TaskRun(run) => format!(
                "[ctl] task {} status={}",
                run.task_id.as_str(),
                task_run_status_label(run.status)
            ),
            ControlEntry::TaskPlan(plan) => format!(
                "[ctl] plan {} v{} status={} steps={}",
                plan.task_id.as_str(),
                plan.plan_version,
                task_plan_status_label(plan.status),
                plan.steps.len()
            ),
            ControlEntry::TaskStep(step) => format!(
                "[ctl] step {} v{}:{} status={}",
                step.task_id.as_str(),
                step.plan_version,
                step.step_id.as_str(),
                task_step_status_label(step.status)
            ),
            ControlEntry::TaskChildSession(child) => format!(
                "[ctl] child {} v{}:{} status={}",
                child.task_id.as_str(),
                child.plan_version,
                child.step_id.as_str(),
                task_child_session_status_label(child.status)
            ),
            ControlEntry::TaskChildSessionDisplayName(rename) => format!(
                "[ctl] child name {} v{}:{} {}",
                rename.child_task_id.as_str(),
                rename.plan_version,
                rename.step_id.as_str(),
                truncate_session_view_text(&rename.display_name, 48)
            ),
            ControlEntry::TaskSubagentApprovalRoute(route) => format!(
                "[ctl] subagent approval {} call={} status={}",
                route.route_id.as_str(),
                route.call_id,
                task_route_status_label(route.status)
            ),
            ControlEntry::TaskSubagentElicitationRoute(route) => format!(
                "[ctl] subagent elicitation {} server={} status={}",
                route.route_id.as_str(),
                route.server_name,
                task_route_status_label(route.status)
            ),
            ControlEntry::AgentProfileCaptured(entry) => format!(
                "[ctl] agent profile {} trust={}",
                entry.snapshot.profile_id.as_str(),
                agent_trust_state_label(entry.snapshot.trust_state)
            ),
            ControlEntry::AgentThreadStarted(entry) => format!(
                "[ctl] agent {} started profile={} mode={}",
                entry.thread_id.as_str(),
                entry.profile_id.as_str(),
                agent_invocation_mode_label(entry.invocation_mode)
            ),
            ControlEntry::AgentThreadStatusChanged(entry) => format!(
                "[ctl] agent {} status={}",
                entry.thread_id.as_str(),
                agent_thread_status_label(entry.status)
            ),
            ControlEntry::AgentThreadMessageRouted(entry) => format!(
                "[ctl] agent message {} status={}",
                entry.route_id.as_str(),
                agent_route_status_label(entry.status)
            ),
            ControlEntry::AgentThreadResultRecorded(entry) => format!(
                "[ctl] agent result {} status={}",
                entry.result.thread_id.as_str(),
                agent_terminal_status_label(entry.result.status)
            ),
            ControlEntry::AgentThreadDisplayName(entry) => format!(
                "[ctl] agent name {} {}",
                entry.thread_id.as_str(),
                truncate_session_view_text(&entry.display_name, 48)
            ),
            ControlEntry::AgentApprovalRoute(route) => format!(
                "[ctl] agent approval {} call={} status={}",
                route.route_id.as_str(),
                route.call_id,
                agent_route_status_label(route.status)
            ),
            ControlEntry::AgentElicitationRoute(route) => format!(
                "[ctl] agent elicitation {} server={} status={}",
                route.route_id.as_str(),
                route.server_name,
                agent_route_status_label(route.status)
            ),
            ControlEntry::AgentRunAttemptStarted(entry) => format!(
                "[ctl] agent attempt {} thread={} model={}",
                entry.attempt_id.as_str(),
                entry.thread_id.as_str(),
                truncate_session_view_text(&entry.model, 32)
            ),
            ControlEntry::AgentRunHeartbeat(entry) => format!(
                "[ctl] agent heartbeat {} thread={} at={}",
                entry.attempt_id.as_str(),
                entry.thread_id.as_str(),
                entry.updated_at_ms
            ),
            ControlEntry::AgentRunInterrupted(entry) => format!(
                "[ctl] agent interrupted {} thread={}",
                entry.attempt_id.as_str(),
                entry.thread_id.as_str()
            ),
            ControlEntry::AgentRouteClosed(entry) => {
                format!("[ctl] agent route {} closed", entry.route_id.as_str())
            }
            ControlEntry::AgentMergeSafePoint(entry) => format!(
                "[ctl] agent merge {} parent={}",
                entry.thread_id.as_str(),
                entry.parent_thread_id.as_str()
            ),
            ControlEntry::AgentThreadClosed(entry) => {
                format!("[ctl] agent {} closed", entry.thread_id.as_str())
            }
            ControlEntry::Note { kind, .. } => format!("[ctl] note {kind}"),
        },
    }
}

fn render_tool_egress_line(egress: &ToolEgressEntry) -> String {
    format!(
        "[ctl] egress {} {} dest={} op={} redacted={}",
        egress.call_id,
        egress.tool_name,
        truncate_session_view_text(&egress.destination, 48),
        truncate_session_view_text(&egress.operation, 32),
        egress.redacted
    )
}

fn restored_tool_execution_index(
    entries: &[SessionLogEntry],
) -> HashMap<String, ToolExecutionEntry> {
    let mut executions = HashMap::new();
    for entry in entries {
        if let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry {
            executions.insert(execution.call_id.clone(), execution.as_ref().clone());
        }
    }
    executions
}

fn restored_tool_preview_snapshot_index(
    entries: &[SessionLogEntry],
) -> HashMap<String, ToolPreviewSnapshot> {
    let mut snapshots = HashMap::new();
    for entry in entries {
        if let SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot)) = entry {
            snapshots.insert(snapshot.call_id.clone(), snapshot.clone());
        }
    }
    snapshots
}

fn restored_tool_result_call_ids(entries: &[SessionLogEntry]) -> HashSet<String> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResult(message) => message.tool_call_id.clone(),
            _ => None,
        })
        .collect()
}

fn should_render_restored_tool_execution(
    execution: &ToolExecutionEntry,
    tool_result_call_ids: &HashSet<String>,
) -> bool {
    !tool_result_call_ids.contains(&execution.call_id)
        && matches!(
            execution.status,
            ToolExecutionStatus::Failed
                | ToolExecutionStatus::Cancelled
                | ToolExecutionStatus::Interrupted
        )
}

fn restored_tool_execution_content(execution: &ToolExecutionEntry) -> String {
    execution
        .error
        .as_ref()
        .map(|error| error.message.clone())
        .unwrap_or_else(|| {
            format!(
                "tool execution ended with status {} before a tool result was written",
                tool_execution_status_label(execution.status)
            )
        })
}

fn restored_reasoning_note(kind: &str, data: &serde_json::Value) -> Option<String> {
    let field = if kind == "reasoning_trace" {
        "text"
    } else {
        "delta"
    };
    data.get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn tool_approval_action_label(action: sigil_kernel::ToolApprovalAuditAction) -> &'static str {
    match action {
        sigil_kernel::ToolApprovalAuditAction::PolicyEvaluated => "policy",
        sigil_kernel::ToolApprovalAuditAction::Requested => "requested",
        sigil_kernel::ToolApprovalAuditAction::Resolved => "resolved",
        sigil_kernel::ToolApprovalAuditAction::PreviewFailed => "preview_failed",
    }
}

fn mcp_elicitation_decision_label(decision: sigil_kernel::McpElicitationDecision) -> &'static str {
    match decision {
        sigil_kernel::McpElicitationDecision::Accepted => "accepted",
        sigil_kernel::McpElicitationDecision::Declined => "declined",
        sigil_kernel::McpElicitationDecision::Cancelled => "cancelled",
    }
}

fn tool_execution_status_label(status: sigil_kernel::ToolExecutionStatus) -> &'static str {
    match status {
        sigil_kernel::ToolExecutionStatus::Started => "started",
        sigil_kernel::ToolExecutionStatus::Completed => "completed",
        sigil_kernel::ToolExecutionStatus::Failed => "failed",
        sigil_kernel::ToolExecutionStatus::Cancelled => "cancelled",
        sigil_kernel::ToolExecutionStatus::Interrupted => "interrupted",
    }
}

fn task_run_status_label(status: sigil_kernel::TaskRunStatus) -> &'static str {
    match status {
        sigil_kernel::TaskRunStatus::Started => "started",
        sigil_kernel::TaskRunStatus::Running => "running",
        sigil_kernel::TaskRunStatus::Paused => "paused",
        sigil_kernel::TaskRunStatus::Completed => "completed",
        sigil_kernel::TaskRunStatus::Failed => "failed",
        sigil_kernel::TaskRunStatus::Cancelled => "cancelled",
        sigil_kernel::TaskRunStatus::Interrupted => "interrupted",
    }
}

fn task_plan_status_label(status: sigil_kernel::TaskPlanStatus) -> &'static str {
    match status {
        sigil_kernel::TaskPlanStatus::Proposed => "proposed",
        sigil_kernel::TaskPlanStatus::Accepted => "accepted",
        sigil_kernel::TaskPlanStatus::Superseded => "superseded",
        sigil_kernel::TaskPlanStatus::Rejected => "rejected",
    }
}

fn task_step_status_label(status: sigil_kernel::TaskStepStatus) -> &'static str {
    match status {
        sigil_kernel::TaskStepStatus::Pending => "pending",
        sigil_kernel::TaskStepStatus::Running => "running",
        sigil_kernel::TaskStepStatus::Completed => "completed",
        sigil_kernel::TaskStepStatus::Failed => "failed",
        sigil_kernel::TaskStepStatus::Blocked => "blocked",
        sigil_kernel::TaskStepStatus::Cancelled => "cancelled",
        sigil_kernel::TaskStepStatus::Interrupted => "interrupted",
    }
}

fn task_child_session_status_label(status: sigil_kernel::TaskChildSessionStatus) -> &'static str {
    match status {
        sigil_kernel::TaskChildSessionStatus::Started => "started",
        sigil_kernel::TaskChildSessionStatus::Completed => "completed",
        sigil_kernel::TaskChildSessionStatus::Failed => "failed",
        sigil_kernel::TaskChildSessionStatus::Cancelled => "cancelled",
        sigil_kernel::TaskChildSessionStatus::Interrupted => "interrupted",
        sigil_kernel::TaskChildSessionStatus::Unavailable => "unavailable",
    }
}

fn task_route_status_label(status: sigil_kernel::TaskRouteStatus) -> &'static str {
    match status {
        sigil_kernel::TaskRouteStatus::Registered => "registered",
        sigil_kernel::TaskRouteStatus::Requested => "requested",
        sigil_kernel::TaskRouteStatus::Resolved => "resolved",
        sigil_kernel::TaskRouteStatus::Rejected => "rejected",
        sigil_kernel::TaskRouteStatus::Cancelled => "cancelled",
        sigil_kernel::TaskRouteStatus::Stale => "stale",
    }
}

fn agent_trust_state_label(status: sigil_kernel::AgentTrustState) -> &'static str {
    match status {
        sigil_kernel::AgentTrustState::Trusted => "trusted",
        sigil_kernel::AgentTrustState::NeedsReview => "needs_review",
        sigil_kernel::AgentTrustState::Disabled => "disabled",
        sigil_kernel::AgentTrustState::Unknown => "unknown",
    }
}

fn agent_invocation_mode_label(mode: sigil_kernel::AgentInvocationMode) -> &'static str {
    match mode {
        sigil_kernel::AgentInvocationMode::Foreground => "foreground",
        sigil_kernel::AgentInvocationMode::Background => "background",
        sigil_kernel::AgentInvocationMode::JoinBeforeFinal => "join_before_final",
        sigil_kernel::AgentInvocationMode::Unknown => "unknown",
    }
}

fn agent_thread_status_label(status: sigil_kernel::AgentThreadStatus) -> &'static str {
    match status {
        sigil_kernel::AgentThreadStatus::Started => "started",
        sigil_kernel::AgentThreadStatus::Running => "running",
        sigil_kernel::AgentThreadStatus::Blocked => "blocked",
        sigil_kernel::AgentThreadStatus::Completed => "completed",
        sigil_kernel::AgentThreadStatus::Failed => "failed",
        sigil_kernel::AgentThreadStatus::Cancelled => "cancelled",
        sigil_kernel::AgentThreadStatus::Interrupted => "interrupted",
        sigil_kernel::AgentThreadStatus::Closed => "closed",
        sigil_kernel::AgentThreadStatus::Unavailable => "unavailable",
        sigil_kernel::AgentThreadStatus::Unknown => "unknown",
    }
}

fn agent_terminal_status_label(status: sigil_kernel::AgentThreadTerminalStatus) -> &'static str {
    match status {
        sigil_kernel::AgentThreadTerminalStatus::Completed => "completed",
        sigil_kernel::AgentThreadTerminalStatus::Failed => "failed",
        sigil_kernel::AgentThreadTerminalStatus::Cancelled => "cancelled",
        sigil_kernel::AgentThreadTerminalStatus::Interrupted => "interrupted",
        sigil_kernel::AgentThreadTerminalStatus::Unknown => "unknown",
    }
}

fn agent_route_status_label(status: sigil_kernel::AgentRouteStatus) -> &'static str {
    match status {
        sigil_kernel::AgentRouteStatus::Registered => "registered",
        sigil_kernel::AgentRouteStatus::Requested => "requested",
        sigil_kernel::AgentRouteStatus::Resolved => "resolved",
        sigil_kernel::AgentRouteStatus::Rejected => "rejected",
        sigil_kernel::AgentRouteStatus::Cancelled => "cancelled",
        sigil_kernel::AgentRouteStatus::Stale => "stale",
        sigil_kernel::AgentRouteStatus::Closed => "closed",
        sigil_kernel::AgentRouteStatus::Unknown => "unknown",
    }
}

fn render_compaction_preview_lines(preview: &CompactionPreview) -> Vec<String> {
    let mut lines = vec![
        format!(
            "/compact preview: fold {}",
            preview.record.compacted_message_count
        ),
        "Before:".to_owned(),
    ];
    for message in &preview.folded_messages {
        lines.push(format!("  {}", render_model_message_line(message)));
    }
    lines.push("After:".to_owned());
    for message in &preview.projected_messages {
        lines.push(format!("  {}", render_model_message_line(message)));
    }
    lines
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/session_flow_detail_tests.rs"]
mod tests;
