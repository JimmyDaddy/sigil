use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sigil_kernel::{
    AssistantMessageKind, CompactionPreview, ControlEntry, JsonlSessionStore, ModelMessage,
    RootConfig, Session, SessionLogEntry, ToolCall, ToolEgressEntry, ToolExecutionEntry,
    ToolExecutionStatus, ToolPreviewSnapshot, inspect_memory_documents, latest_compaction_record,
    session_stats_from_entries,
};
use uuid::Uuid;

use super::{
    AppState, PaneFocus, RunPhase, SESSION_HISTORY_TITLE_SCAN_LIMIT, SessionHistoryEntry,
    SessionHistoryRow, SessionViewMode, TimelineRole,
    formatting::{
        agent_result_poll_tool_name, format_agent_thread_started_block,
        format_agent_thread_status_block, format_terminal_task_block_redacted,
        format_tool_content_block_redacted_for_restore, human_file_size, relative_age_label,
        truncate_session_view_text,
    },
    worker_bridge::tool_card_replacement_key,
};
use crate::view_model::{RecoveryPanelViewModel, TaskMemoryInspectViewModel};

const SESSION_HISTORY_TITLE_LINE_MAX_BYTES: usize = 256 * 1024;
impl AppState {
    pub fn restore_latest_session_from_disk(&mut self, root_config: &RootConfig) -> bool {
        self.refresh_session_history();
        let Some(session_log_path) = self
            .session_browser
            .history
            .first()
            .map(|entry| entry.path.clone())
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
            format!("{} view", self.session_browser.view_mode.label()),
            format!(
                "compact={}  prompt={}  cache={:.0}%",
                self.runtime.compaction_status,
                self.runtime.stats.last_prompt_tokens,
                self.cache_hit_ratio() * 100.0
            ),
        ];
        if self.runtime.is_busy {
            lines.push("running; durable view".to_owned());
        }

        lines.push(String::new());
        lines.extend(match self.session_browser.view_mode {
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
        if self.session_browser.current_entries.is_empty() {
            return vec!["no provider messages".to_owned()];
        }

        let session = Session::from_entries(
            self.runtime.provider_name.clone(),
            self.runtime.model_name.clone(),
            self.session_browser.current_entries.clone(),
        );
        let messages = session.messages();
        let mut lines = vec!["Provider:".to_owned()];
        if let Some(panel) = RecoveryPanelViewModel::from_entries(
            &self.session_browser.current_entries,
            unix_time_ms(),
        ) {
            lines.extend(panel.lines().into_iter().map(|line| format!("  {line}")));
        }
        if let Some(record) = &self.latest_compaction_record {
            lines.push(format!(
                "  summary: compacted={} tail={}",
                record.compacted_message_count, record.retained_tail_message_count
            ));
            if let Some(task_memory) = &record.task_memory {
                lines.extend(
                    TaskMemoryInspectViewModel::from_task_memory(task_memory)
                        .lines()
                        .into_iter()
                        .map(|line| format!("  {line}")),
                );
            }
        }
        for message in messages {
            lines.push(render_model_message_line(&message));
        }
        if !self.runtime.is_busy {
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
        if self.session_browser.current_entries.is_empty() {
            return vec!["no audit entries".to_owned()];
        }

        let mut lines = vec!["Audit:".to_owned()];
        for entry in &self.session_browser.current_entries {
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
            filter: if self.session_browser.history_filter.is_empty() {
                "-".to_owned()
            } else {
                self.session_browser.history_filter.clone()
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
            .session_browser
            .history_selected
            .saturating_sub(self.session_browser.history_visible_limit / 2)
            .min(filtered_indices.len().saturating_sub(1));
        let end = (start + self.session_browser.history_visible_limit).min(filtered_indices.len());
        for (filtered_index, entry_index) in filtered_indices
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            let entry = &self.session_browser.history[*entry_index];
            rows.push(SessionHistoryRow::SessionItem {
                index: filtered_index + 1,
                label: session_history_display_label(entry),
                current: entry.path == self.session_log_path,
                selected: filtered_index == self.session_browser.history_selected,
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
        let entries =
            preserve_local_ui_control_entries(&self.session_browser.current_entries, entries);
        self.runtime.stats = session_stats_from_entries(&entries);
        self.latest_compaction_record = latest_compaction_record(&entries);
        self.tool_preview_snapshots = restored_tool_preview_snapshot_index(&entries);
        self.session_browser.current_entries = entries;
        self.mark_current_session_entries_changed();
        self.refresh_conversation_queue_selection();
        self.refresh_active_agent_view_after_parent_sync();
        self.refresh_usage_sidebar_cache();
    }

    pub(super) fn append_current_session_control(&mut self, control: ControlEntry) {
        self.session_browser
            .current_entries
            .push(SessionLogEntry::Control(control));
        self.runtime.stats = session_stats_from_entries(&self.session_browser.current_entries);
        self.latest_compaction_record =
            latest_compaction_record(&self.session_browser.current_entries);
        self.tool_preview_snapshots =
            restored_tool_preview_snapshot_index(&self.session_browser.current_entries);
        self.mark_current_session_entries_changed();
        self.refresh_conversation_queue_selection();
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
        self.session_browser.history = sessions.into_iter().map(|(_, entry)| entry).collect();
        let current_index = self
            .session_browser
            .history
            .iter()
            .position(|entry| entry.path == self.session_log_path)
            .unwrap_or(0);
        self.session_browser.history_selected = self
            .filtered_session_indices()
            .iter()
            .position(|index| *index == current_index)
            .unwrap_or(0)
            .min(self.filtered_session_indices().len().saturating_sub(1));
    }

    pub(super) fn refresh_memory_summary(&mut self) {
        match inspect_memory_documents(&self.workspace_root, &self.memory_config) {
            Ok(report) => {
                self.runtime.memory_enabled = report.enabled;
                self.runtime.memory_document_count = report.document_count;
                self.runtime.memory_last_status = "ok".to_owned();
            }
            Err(error) => {
                self.runtime.memory_enabled = self.memory_config.enabled;
                self.runtime.memory_document_count = 0;
                self.runtime.memory_last_status = error.to_string();
            }
        }
    }

    pub(super) fn resolve_resume_target(&self, selector: &str) -> Option<PathBuf> {
        if self.session_browser.history.is_empty() {
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
                .and_then(|index| self.session_browser.history.get(*index))
                .map(|entry| entry.path.clone());
        }

        if let Some(path) = normalized
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| candidate_indices.get(index).copied())
            .and_then(|index| self.session_browser.history.get(index))
            .map(|entry| entry.path.clone())
        {
            return Some(path);
        }

        let path = PathBuf::from(normalized);
        if self
            .session_browser
            .history
            .iter()
            .any(|entry| entry.path == path)
        {
            return Some(path);
        }

        let query = normalized.to_ascii_lowercase();
        let mut matches = candidate_indices
            .into_iter()
            .filter_map(|index| self.session_browser.history.get(index))
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
            .session_browser
            .history
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| (entry.path != self.session_log_path).then_some(index))
            .collect::<Vec<_>>();
        if non_current.is_empty() {
            return (0..self.session_browser.history.len()).collect();
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
        self.runtime.provider_name = provider_name;
        self.runtime.model_name = model_name;
        self.session_id = session_id_from_path(&self.session_log_path)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.active_agent_view = super::AgentView::Main;
        self.active_agent_child_transcript = None;
        self.sync_current_session_state(entries.clone());
        self.approval.pending = None;
        self.runtime.run_phase = RunPhase::Idle;
        self.refresh_memory_summary();
        self.recompute_compaction_status(false);
        self.timeline.clear();
        self.tool_activity_cache.clear();
        self.tool_activity_visible_rows.clear();
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
            format!("{}/{}", self.runtime.provider_name, self.runtime.model_name),
        );
        self.push_event("effort", self.runtime.reasoning_effort.as_str());
        self.push_event(
            "approval_default",
            self.runtime.permission_default_mode.clone(),
        );
        self.push_event(
            "memory",
            format!(
                "enabled={} docs={} status={}",
                self.runtime.memory_enabled,
                self.runtime.memory_document_count,
                self.runtime.memory_last_status
            ),
        );
        self.push_event("compaction", self.runtime.compaction_status.clone());
        self.push_event("session_log", self.session_log_path.display().to_string());
        self.push_event("focus", self.active_pane.label());
        self.push_event("restore", format!("entries={}", entries.len()));

        let restored_tool_executions = restored_tool_execution_index(&entries);
        let restored_tool_calls = restored_tool_call_index(&entries);
        let restored_tool_previews = restored_tool_preview_snapshot_index(&entries);
        let restored_tool_result_call_ids = restored_tool_result_call_ids(&entries);
        let suppressed_reasoning_trace_indices = suppressed_reasoning_trace_indices(&entries);
        let suppressed_assistant_preamble_indices = suppressed_assistant_preamble_indices(&entries);
        self.tool_preview_snapshots = restored_tool_previews.clone();
        for (entry_index, entry) in entries.into_iter().enumerate() {
            match entry {
                SessionLogEntry::User(message) => {
                    if let Some(content) = message.content {
                        self.push_timeline(TimelineRole::User, content);
                    }
                }
                SessionLogEntry::Assistant(message) => {
                    if !suppressed_assistant_preamble_indices.contains(&entry_index)
                        && let Some(content) = message.content
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
                        let tool_call = message
                            .tool_call_id
                            .as_deref()
                            .and_then(|call_id| restored_tool_calls.get(call_id));
                        self.replace_or_push_tool_card(
                            format_tool_content_block_redacted_for_restore(
                                message.tool_call_id.as_deref(),
                                &content,
                                execution,
                                tool_call,
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
                        if !suppressed_reasoning_trace_indices.contains(&entry_index)
                            && let Some(delta) = restored_reasoning_note(&kind, &data)
                        {
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
                        let tool_call = restored_tool_calls.get(&execution.call_id);
                        self.replace_or_push_tool_card(
                            format_tool_content_block_redacted_for_restore(
                                Some(execution.call_id.as_str()),
                                &restored_tool_execution_content(execution.as_ref()),
                                Some(execution.as_ref()),
                                tool_call,
                                preview,
                                &self.secret_redactor,
                            ),
                        );
                        self.push_event("control:restore", render_tool_execution_line(&execution));
                    }
                    ControlEntry::TerminalTask(task) => {
                        self.replace_or_push_tool_card(format_terminal_task_block_redacted(
                            &task,
                            &self.secret_redactor,
                        ));
                        self.push_event(
                            "control:restore",
                            format!(
                                "terminal {} status={}",
                                task.handle.task_id.as_str(),
                                task.status.as_str()
                            ),
                        );
                    }
                    ControlEntry::AgentThreadStarted(entry) => {
                        self.replace_or_push_tool_card(format_agent_thread_started_block(&entry));
                        self.push_event(
                            "control:restore",
                            format!(
                                "agent {} started profile={}",
                                entry.thread_id.as_str(),
                                entry.profile_id.as_str()
                            ),
                        );
                    }
                    ControlEntry::AgentThreadStatusChanged(entry) => {
                        self.replace_or_push_tool_card(format_agent_thread_status_block(&entry));
                        self.push_event(
                            "control:restore",
                            format!(
                                "agent {} status={:?}",
                                entry.thread_id.as_str(),
                                entry.status
                            ),
                        );
                    }
                    other => {
                        self.push_event("control:restore", render_control_entry_line(&other));
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
        if delta.trim().is_empty() {
            return;
        }
        self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
        self.push_timeline(TimelineRole::Thinking, delta.to_owned());
    }

    pub(super) fn restored_timeline_entries_from_session_entries(
        &self,
        entries: &[SessionLogEntry],
    ) -> Vec<crate::timeline::TimelineEntry> {
        restored_timeline_entries_from_session_entries(entries, &self.secret_redactor)
    }

    pub(super) fn filtered_session_indices(&self) -> Vec<usize> {
        let filter = self.session_browser.history_filter.to_ascii_lowercase();
        self.session_browser
            .history
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
    let restored_tool_calls = restored_tool_call_index(entries);
    let restored_tool_previews = restored_tool_preview_snapshot_index(entries);
    let restored_tool_result_call_ids = restored_tool_result_call_ids(entries);
    let suppressed_reasoning_trace_indices = suppressed_reasoning_trace_indices(entries);
    let suppressed_assistant_preamble_indices = suppressed_assistant_preamble_indices(entries);
    let mut timeline = Vec::new();
    for (entry_index, entry) in entries.iter().enumerate() {
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
                if !suppressed_assistant_preamble_indices.contains(&entry_index)
                    && let Some(content) = message.content.as_ref()
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
                    let tool_call = message
                        .tool_call_id
                        .as_deref()
                        .and_then(|call_id| restored_tool_calls.get(call_id));
                    push_restored_tool_card(
                        &mut timeline,
                        format_tool_content_block_redacted_for_restore(
                            message.tool_call_id.as_deref(),
                            content,
                            execution,
                            tool_call,
                            preview,
                            redactor,
                        ),
                    );
                }
            }
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if kind == "reasoning_delta" || kind == "reasoning_trace" =>
            {
                if !suppressed_reasoning_trace_indices.contains(&entry_index)
                    && let Some(delta) = restored_reasoning_note(kind, data)
                {
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
                let tool_call = restored_tool_calls.get(&execution.call_id);
                push_restored_tool_card(
                    &mut timeline,
                    format_tool_content_block_redacted_for_restore(
                        Some(execution.call_id.as_str()),
                        &restored_tool_execution_content(execution.as_ref()),
                        Some(execution.as_ref()),
                        tool_call,
                        preview,
                        redactor,
                    ),
                );
            }
            SessionLogEntry::Control(ControlEntry::TerminalTask(task)) => {
                push_restored_tool_card(
                    &mut timeline,
                    format_terminal_task_block_redacted(task, redactor),
                );
            }
            SessionLogEntry::Control(ControlEntry::AgentThreadStarted(entry)) => {
                push_restored_tool_card(&mut timeline, format_agent_thread_started_block(entry));
            }
            SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(entry)) => {
                push_restored_tool_card(&mut timeline, format_agent_thread_status_block(entry));
            }
            SessionLogEntry::Control(_) => {}
        }
    }
    timeline
}

fn push_restored_tool_card(timeline: &mut Vec<crate::timeline::TimelineEntry>, text: String) {
    let Some(current_key) = tool_card_replacement_key(&text) else {
        timeline.push(crate::timeline::TimelineEntry {
            role: TimelineRole::Tool,
            text,
        });
        return;
    };
    let mut matching_indices = timeline
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            (entry.role == TimelineRole::Tool
                && tool_card_replacement_key(&entry.text)
                    .is_some_and(|previous_key| previous_key == current_key))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let Some(keep_index) = matching_indices.first().copied() else {
        timeline.push(crate::timeline::TimelineEntry {
            role: TimelineRole::Tool,
            text,
        });
        return;
    };
    timeline[keep_index].text = text;
    matching_indices.remove(0);
    for index in matching_indices.into_iter().rev() {
        timeline.remove(index);
    }
}

fn suppressed_reasoning_trace_indices(entries: &[SessionLogEntry]) -> HashSet<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            restored_reasoning_note_entry(entry)
                .then_some(())
                .filter(|_| {
                    reasoning_trace_is_in_turn_before_final(entries, index)
                        || reasoning_trace_is_immediately_before_agent_poll(entries, index)
                })
                .map(|_| index)
        })
        .collect()
}

fn reasoning_trace_is_in_turn_before_final(entries: &[SessionLogEntry], index: usize) -> bool {
    entries
        .iter()
        .skip(index.saturating_add(1))
        .take_while(|entry| !matches!(entry, SessionLogEntry::User(_)))
        .any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Assistant(message) if assistant_message_is_final_answer(message)
            )
        })
}

fn reasoning_trace_is_immediately_before_agent_poll(
    entries: &[SessionLogEntry],
    index: usize,
) -> bool {
    for entry in entries.iter().skip(index.saturating_add(1)) {
        if restored_reasoning_note_entry(entry) {
            continue;
        }
        return matches!(
            entry,
            SessionLogEntry::Assistant(message)
                if assistant_message_calls_suppressed_agent_poll(message)
        );
    }
    false
}

fn suppressed_assistant_preamble_indices(entries: &[SessionLogEntry]) -> HashSet<usize> {
    let final_answer_indices = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            SessionLogEntry::Assistant(message) if assistant_message_is_final_answer(message) => {
                Some(index)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if final_answer_indices.is_empty() {
        return HashSet::new();
    }
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let SessionLogEntry::Assistant(message) = entry else {
                return None;
            };
            let has_preamble = assistant_message_is_tool_preamble(message)
                || !message.tool_calls.is_empty()
                    && message
                        .content
                        .as_ref()
                        .is_some_and(|content| !content.trim().is_empty());
            (has_preamble
                && final_answer_indices
                    .iter()
                    .any(|final_index| *final_index > index))
            .then_some(index)
        })
        .collect()
}

fn restored_reasoning_note_entry(entry: &SessionLogEntry) -> bool {
    matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::Note { kind, .. })
            if kind == "reasoning_delta" || kind == "reasoning_trace"
    )
}

fn assistant_message_is_final_answer(message: &ModelMessage) -> bool {
    if message.assistant_kind == Some(AssistantMessageKind::FinalAnswer) {
        return true;
    }
    if message.assistant_kind.is_some() {
        return false;
    }
    message.tool_calls.is_empty()
        && message
            .content
            .as_ref()
            .is_some_and(|content| !content.trim().is_empty())
}

fn assistant_message_is_tool_preamble(message: &ModelMessage) -> bool {
    message.assistant_kind == Some(AssistantMessageKind::ToolPreamble)
}

fn assistant_message_calls_suppressed_agent_poll(message: &ModelMessage) -> bool {
    message
        .tool_calls
        .iter()
        .any(|call| agent_result_poll_tool_name(call.name.as_str()))
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
    if delta.trim().is_empty() {
        return;
    }
    timeline.push(crate::timeline::TimelineEntry {
        role: TimelineRole::Thinking,
        text: delta.to_owned(),
    });
}

fn preserve_local_ui_control_entries(
    current_entries: &[SessionLogEntry],
    mut incoming_entries: Vec<SessionLogEntry>,
) -> Vec<SessionLogEntry> {
    for entry in current_entries
        .iter()
        .filter(|entry| is_local_ui_control_entry(entry))
    {
        if !has_equivalent_local_ui_control_entry(&incoming_entries, entry) {
            incoming_entries.push(entry.clone());
        }
    }
    incoming_entries
}

fn has_equivalent_local_ui_control_entry(
    entries: &[SessionLogEntry],
    target: &SessionLogEntry,
) -> bool {
    entries
        .iter()
        .any(|entry| local_ui_control_entries_equal(entry, target))
}

fn is_local_ui_control_entry(entry: &SessionLogEntry) -> bool {
    matches!(
        entry,
        SessionLogEntry::Control(
            ControlEntry::AgentThreadClosed(_)
                | ControlEntry::AgentThreadDisplayName(_)
                | ControlEntry::TaskChildSessionDisplayName(_)
        )
    )
}

fn local_ui_control_entries_equal(left: &SessionLogEntry, right: &SessionLogEntry) -> bool {
    match (left, right) {
        (
            SessionLogEntry::Control(ControlEntry::AgentThreadClosed(left)),
            SessionLogEntry::Control(ControlEntry::AgentThreadClosed(right)),
        ) => left.thread_id == right.thread_id && left.reason == right.reason,
        (
            SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(left)),
            SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(right)),
        ) => left.thread_id == right.thread_id && left.display_name == right.display_name,
        (
            SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(left)),
            SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(right)),
        ) => {
            left.task_id == right.task_id
                && left.plan_version == right.plan_version
                && left.step_id == right.step_id
                && left.child_task_id == right.child_task_id
                && left.display_name == right.display_name
        }
        _ => false,
    }
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

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn render_session_log_entry(entry: &SessionLogEntry) -> String {
    match entry {
        SessionLogEntry::User(message)
        | SessionLogEntry::Assistant(message)
        | SessionLogEntry::ToolResult(message) => render_model_message_line(message),
        SessionLogEntry::Control(control) => render_control_entry_line(control),
    }
}

pub(super) fn render_control_entry_line(control: &ControlEntry) -> String {
    match control {
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
        ControlEntry::ContextAssemblySkipped(skipped) => format!(
            "[ctl] context skipped candidates={} items={} reason={}",
            skipped.candidate_count,
            skipped.item_ids.len(),
            truncate_session_view_text(&skipped.reason, 96)
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
        ControlEntry::ToolApprovalSessionGrant(grant) => format!(
            "[ctl] approval grant {} {} scope=session subjects={}",
            grant.call_id,
            grant.tool_name,
            grant.subjects.len()
        ),
        ControlEntry::ToolExecution(execution) => render_tool_execution_line(execution),
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
        ControlEntry::PluginHookExecutionStarted(entry) => format!(
            "[ctl] plugin hook {}:{} started kind={} effect={} backend={} profile={} coverage={}",
            truncate_session_view_text(&entry.plugin_id, 32),
            truncate_session_view_text(&entry.hook_id, 32),
            format!("{:?}", entry.hook_kind).to_ascii_lowercase(),
            entry.declared_effect.as_str(),
            entry.backend.as_str(),
            entry.sandbox_profile.as_str(),
            entry.execution_coverage.as_str()
        ),
        ControlEntry::PluginHookExecutionFinished(entry) => format!(
            "[ctl] plugin hook {}:{} finished status={} exit={} stdout={} stderr={} backend={} network={}",
            truncate_session_view_text(&entry.plugin_id, 32),
            truncate_session_view_text(&entry.hook_id, 32),
            plugin_hook_execution_status_label(entry.status),
            entry
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "none".to_owned()),
            entry.stdout_bytes,
            entry.stderr_bytes,
            entry.backend.as_str(),
            entry.network.policy.as_str()
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
        ControlEntry::WriteLeaseAcquired(entry) => format!(
            "[ctl] write lease {} acquired isolation={} scope={} owner={}",
            truncate_session_view_text(entry.lease_id.as_str(), 48),
            entry.isolation_mode.as_str(),
            write_lease_scope_label(&entry.scope),
            truncate_session_view_text(&entry.owner_agent_id, 48)
        ),
        ControlEntry::WriteLeaseReleased(entry) => format!(
            "[ctl] write lease {} released status={}",
            truncate_session_view_text(entry.lease_id.as_str(), 48),
            entry.status.as_str()
        ),
        ControlEntry::IsolatedWorkspaceCreated(entry) => format!(
            "[ctl] isolated workspace {} backend={} mode={} base={}",
            truncate_session_view_text(&entry.isolated_workspace_id, 48),
            entry.backend.as_str(),
            entry.isolation_mode.as_str(),
            truncate_session_view_text(&entry.base_snapshot_id, 16)
        ),
        ControlEntry::IsolatedChangeSetProduced(entry) => format!(
            "[ctl] isolated changeset {} mode={} subjects={} artifact={}",
            entry.changeset_id.as_str(),
            entry.source_isolation.as_str(),
            entry.touched_subjects.len(),
            truncate_session_view_text(entry.artifact_ref.as_deref().unwrap_or("-"), 48)
        ),
        ControlEntry::MergeReviewRequested(entry) => format!(
            "[ctl] merge review {} changeset={} snapshot={}",
            truncate_session_view_text(entry.review_id.as_str(), 48),
            entry.changeset_id.as_str(),
            truncate_session_view_text(&entry.parent_workspace_snapshot_id, 16)
        ),
        ControlEntry::MergeReviewResolved(entry) => format!(
            "[ctl] merge review {} decision={} reason={}",
            truncate_session_view_text(entry.review_id.as_str(), 48),
            entry.decision.as_str(),
            truncate_session_view_text(entry.reason.as_deref().unwrap_or("-"), 64)
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
        ControlEntry::PlanApproved(entry) => format!(
            "[ctl] legacy plan grant v{} permission={} expires={} hash={}",
            entry.plan_version,
            plan_approval_permission_label(entry.permission),
            plan_approval_expiry_label(&entry.expires),
            truncate_session_view_text(&entry.plan_hash, 16)
        ),
        ControlEntry::PlanDraftCreated(entry) => format!(
            "[ctl] plan draft {} paths={} suggested_checks={} hash={}",
            entry.plan_id.as_str(),
            entry.target_paths.len(),
            entry.suggested_checks.len(),
            truncate_session_view_text(&entry.plan_hash, 16)
        ),
        ControlEntry::PlanDecisionRecorded(entry) => format!(
            "[ctl] plan decision {} decision={} hash={} reason={}",
            entry.plan_id.as_str(),
            entry.decision.as_str(),
            truncate_session_view_text(&entry.plan_hash, 16),
            truncate_session_view_text(entry.reason.as_deref().unwrap_or("-"), 48)
        ),
        ControlEntry::PlanPermissionGranted(entry) => format!(
            "[ctl] plan grant {} task={} permission={} paths={} snapshot={}",
            entry.plan_id.as_str(),
            entry.task_id.as_str(),
            plan_approval_permission_label(entry.permission),
            entry.scope.workspace_paths.len(),
            truncate_session_view_text(entry.workspace_snapshot_id.as_deref().unwrap_or("-"), 16)
        ),
        ControlEntry::TaskCreatedFromPlan(entry) => {
            let plan_state = if entry.task_plan_version == 0 {
                "task_plan=pending".to_owned()
            } else {
                format!(
                    "task_plan=v{} mappings={}",
                    entry.task_plan_version,
                    entry.step_mapping.len()
                )
            };
            format!(
                "[ctl] task from plan plan={} task={} {} stale={}",
                entry.plan_id.as_str(),
                entry.task_id.as_str(),
                plan_state,
                truncate_session_view_text(entry.stale_reason.as_deref().unwrap_or("-"), 48)
            )
        }
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
        ControlEntry::JobIntentRecorded(entry) => format!(
            "[ctl] job intent {} effect={} policy={}",
            truncate_session_view_text(&entry.job_id, 32),
            entry.expected_effect.as_str(),
            truncate_session_view_text(&entry.tool_policy_hash, 16)
        ),
        ControlEntry::StepLeaseRecorded(entry) => format!(
            "[ctl] step lease {} job={} status={} owner={}",
            truncate_session_view_text(&entry.lease_id, 24),
            truncate_session_view_text(&entry.job_id, 24),
            step_lease_status_label(entry.status),
            truncate_session_view_text(&entry.owner_process_id, 24)
        ),
        ControlEntry::StepLeaseHeartbeatRecorded(entry) => format!(
            "[ctl] step lease heartbeat {} job={} at={} deadline={}",
            truncate_session_view_text(&entry.lease_id, 24),
            truncate_session_view_text(&entry.job_id, 24),
            entry.observed_at_ms,
            entry.next_deadline_ms
        ),
        ControlEntry::CheckSpecRecorded(entry) => format!(
            "[ctl] check spec {} source={} promotion={}",
            truncate_session_view_text(&entry.trusted_check.check_spec.check_spec_id, 48),
            check_discovery_source_label(entry.trusted_check.source),
            check_promotion_label(&entry.trusted_check.promoted_by)
        ),
        ControlEntry::VerificationPolicyChanged(entry) => format!(
            "[ctl] verification policy {} checks={} hash={}",
            evidence_scope_label(&entry.scope),
            entry.policy.required_checks.len(),
            truncate_session_view_text(&entry.policy_hash, 16)
        ),
        ControlEntry::VerificationCheckRun(entry) => format!(
            "[ctl] verification check run {} check={} status={} timeout={} receipt={} reason={}",
            truncate_session_view_text(&entry.run_id, 48),
            truncate_session_view_text(&entry.check_spec_id, 48),
            verification_check_run_status_label(entry.status),
            entry
                .timeout_ms
                .map(|value| format!("{value}ms"))
                .unwrap_or_else(|| "-".to_owned()),
            truncate_session_view_text(entry.receipt_id.as_deref().unwrap_or("-"), 48),
            truncate_session_view_text(entry.reason.as_deref().unwrap_or("-"), 64)
        ),
        ControlEntry::VerificationRecorded(entry) => format!(
            "[ctl] verification receipt {} check={} status={} snapshot={} policy={} trust={}",
            truncate_session_view_text(&entry.receipt.receipt.receipt_id, 48),
            truncate_session_view_text(&entry.receipt.check_spec_id, 48),
            receipt_status_label(entry.receipt.check_status),
            truncate_session_view_text(&entry.receipt.binding.workspace_snapshot_id, 16),
            truncate_session_view_text(
                entry.receipt.receipt.policy_hash.as_deref().unwrap_or("-"),
                16
            ),
            truncate_session_view_text(&entry.receipt.binding.workspace_trust_snapshot_id, 16)
        ),
        ControlEntry::ReadinessEvaluated(entry) => format!(
            "[ctl] readiness {} run={} verification={} policy={} snapshot={} actions={} reasons={}",
            evidence_scope_label(&entry.scope),
            run_status_label(entry.evaluation.run_status),
            verification_verdict_label(entry.evaluation.verification_verdict),
            truncate_session_view_text(entry.policy_hash.as_deref().unwrap_or("-"), 16),
            truncate_session_view_text(entry.workspace_snapshot_id.as_deref().unwrap_or("-"), 16),
            readiness_required_actions_label(&entry.evaluation.required_actions),
            readiness_reasons_label(&entry.evaluation.reasons)
        ),
        ControlEntry::ChildVerificationReceiptLinked(entry) => format!(
            "[ctl] child verification receipt {} child={} status={} parent_recheck={} snapshot={}",
            truncate_session_view_text(&entry.child_receipt_id, 48),
            truncate_session_view_text(&entry.child_session_id, 48),
            child_verification_link_status_label(entry),
            child_verification_parent_recheck_label(entry),
            truncate_session_view_text(&entry.child_workspace_snapshot_id, 16)
        ),
        ControlEntry::WorkspaceTrustDecision(entry) => format!(
            "[ctl] workspace trust {} trust={} snapshot={} by={} reason={}",
            truncate_session_view_text(&entry.workspace_id, 48),
            workspace_trust_label(entry.trust),
            truncate_session_view_text(&entry.workspace_trust_snapshot_id, 16),
            truncate_session_view_text(entry.decided_by_event_id.as_deref().unwrap_or("-"), 48),
            truncate_session_view_text(entry.reason.as_deref().unwrap_or("-"), 64)
        ),
        ControlEntry::AgentProfileCaptured(entry) => format!(
            "[ctl] agent profile {} trust={}",
            entry.snapshot.profile_id.as_str(),
            agent_trust_state_label(entry.snapshot.trust_state)
        ),
        ControlEntry::AgentProfileTrustDecision(entry) => format!(
            "[ctl] agent profile {} trust={} hash={}",
            entry.profile_id.as_str(),
            agent_trust_state_label(entry.decision),
            truncate_session_view_text(&entry.profile_hash, 16)
        ),
        ControlEntry::AgentProfilePolicyDecision(entry) => format!(
            "[ctl] agent profile {} policy enabled={} user={} model={} hash={}",
            entry.profile_id.as_str(),
            optional_bool_label(entry.enabled),
            optional_bool_label(entry.user_invocable),
            optional_bool_label(entry.model_invocable),
            truncate_session_view_text(&entry.profile_hash, 16)
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
        ControlEntry::AgentMailboxMessage(entry) => format!(
            "[ctl] agent mailbox {} status={}",
            entry.route_id.as_str(),
            agent_mailbox_status_label(entry.status)
        ),
        ControlEntry::AgentThreadResultRecorded(entry) => format!(
            "[ctl] agent result {} status={}",
            entry.result.thread_id.as_str(),
            agent_terminal_status_label(entry.result.status)
        ),
        ControlEntry::AgentThreadResultDelivered(entry) => format!(
            "[ctl] agent result delivered {} call={}",
            entry.thread_id.as_str(),
            entry.call_id
        ),
        ControlEntry::AgentResultContinuation(entry) => format!(
            "[ctl] agent continuation {} status={:?}",
            entry.thread_id.as_str(),
            entry.status
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
        ControlEntry::ConversationInputQueued(entry) => format!(
            "[ctl] queue {} kind={:?} prompt={}",
            entry.queue_id.as_str(),
            entry.kind,
            truncate_session_view_text(&entry.prompt, 48)
        ),
        ControlEntry::ConversationInputQueueControl(entry) => {
            format!("[ctl] queue control {:?}", entry.action)
        }
        ControlEntry::ConversationInputEdited(entry) => format!(
            "[ctl] queue {} edited prompt={}",
            entry.queue_id.as_str(),
            truncate_session_view_text(&entry.prompt, 48)
        ),
        ControlEntry::ConversationInputReordered(entry) => format!(
            "[ctl] queue {} moved after {}",
            entry.queue_id.as_str(),
            entry
                .after_queue_id
                .as_ref()
                .map_or("front", sigil_kernel::ConversationInputQueueId::as_str)
        ),
        ControlEntry::ConversationInputStatusChanged(entry) => format!(
            "[ctl] queue {} status={:?}",
            entry.queue_id.as_str(),
            entry.status
        ),
        ControlEntry::Note { kind, .. } => format!("[ctl] note {kind}"),
    }
}

fn render_tool_execution_line(execution: &ToolExecutionEntry) -> String {
    format!(
        "[ctl] execution {} {} status={}",
        execution.call_id,
        execution.tool_name,
        tool_execution_status_label(execution.status)
    )
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

fn restored_tool_call_index(entries: &[SessionLogEntry]) -> HashMap<String, ToolCall> {
    let mut calls = HashMap::new();
    for entry in entries {
        if let SessionLogEntry::Assistant(message) = entry {
            for call in &message.tool_calls {
                calls.insert(call.id.clone(), call.clone());
            }
        }
    }
    calls
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

fn plan_approval_permission_label(
    permission: sigil_kernel::PlanApprovalPermission,
) -> &'static str {
    match permission {
        sigil_kernel::PlanApprovalPermission::Ask => "ask",
        sigil_kernel::PlanApprovalPermission::WorkspaceEdits => "workspace_edits",
    }
}

fn plan_approval_expiry_label(expiry: &sigil_kernel::PlanApprovalExpiry) -> &'static str {
    match expiry {
        sigil_kernel::PlanApprovalExpiry::NextUserPrompt => "next_user_prompt",
        sigil_kernel::PlanApprovalExpiry::Session => "session",
        sigil_kernel::PlanApprovalExpiry::AtUnixMs(_) => "at_unix_ms",
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
        sigil_kernel::TaskStepStatus::Superseded => "superseded",
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

fn step_lease_status_label(status: sigil_kernel::StepLeaseStatus) -> &'static str {
    match status {
        sigil_kernel::StepLeaseStatus::Acquired => "acquired",
        sigil_kernel::StepLeaseStatus::Released => "released",
        sigil_kernel::StepLeaseStatus::Interrupted => "interrupted",
        sigil_kernel::StepLeaseStatus::Abandoned => "abandoned",
    }
}

fn write_lease_scope_label(scope: &sigil_kernel::WriteLeaseScope) -> &'static str {
    match scope {
        sigil_kernel::WriteLeaseScope::Workspace => "workspace",
        sigil_kernel::WriteLeaseScope::Subjects(_) => "subjects",
    }
}

fn run_status_label(status: sigil_kernel::RunStatus) -> &'static str {
    match status {
        sigil_kernel::RunStatus::Running => "running",
        sigil_kernel::RunStatus::Completed => "completed",
        sigil_kernel::RunStatus::Paused => "paused",
        sigil_kernel::RunStatus::Blocked => "blocked",
        sigil_kernel::RunStatus::Failed => "failed",
        sigil_kernel::RunStatus::Cancelled => "cancelled",
        sigil_kernel::RunStatus::Interrupted => "interrupted",
    }
}

fn verification_verdict_label(status: sigil_kernel::VerificationVerdict) -> &'static str {
    match status {
        sigil_kernel::VerificationVerdict::NotEvaluated => "not_evaluated",
        sigil_kernel::VerificationVerdict::NotApplicable => "not_applicable",
        sigil_kernel::VerificationVerdict::Pending => "pending",
        sigil_kernel::VerificationVerdict::Passed => "passed",
        sigil_kernel::VerificationVerdict::Failed => "failed",
        sigil_kernel::VerificationVerdict::Missing => "missing",
        sigil_kernel::VerificationVerdict::Inconclusive => "inconclusive",
        sigil_kernel::VerificationVerdict::Stale => "stale",
        sigil_kernel::VerificationVerdict::Skipped => "skipped",
    }
}

fn receipt_status_label(status: sigil_kernel::ReceiptStatus) -> &'static str {
    match status {
        sigil_kernel::ReceiptStatus::Succeeded => "succeeded",
        sigil_kernel::ReceiptStatus::Failed => "failed",
        sigil_kernel::ReceiptStatus::Skipped => "skipped",
        sigil_kernel::ReceiptStatus::Inconclusive => "inconclusive",
    }
}

fn verification_check_run_status_label(
    status: sigil_kernel::VerificationCheckRunStatus,
) -> &'static str {
    match status {
        sigil_kernel::VerificationCheckRunStatus::Queued => "queued",
        sigil_kernel::VerificationCheckRunStatus::Running => "running",
        sigil_kernel::VerificationCheckRunStatus::Succeeded => "succeeded",
        sigil_kernel::VerificationCheckRunStatus::Failed => "failed",
        sigil_kernel::VerificationCheckRunStatus::Skipped => "skipped",
        sigil_kernel::VerificationCheckRunStatus::Inconclusive => "inconclusive",
        sigil_kernel::VerificationCheckRunStatus::Errored => "errored",
    }
}

fn readiness_required_actions_label(actions: &[sigil_kernel::RequiredAction]) -> String {
    summarized_readiness_items(actions, required_action_label)
}

fn readiness_reasons_label(reasons: &[sigil_kernel::ReadinessReason]) -> String {
    summarized_readiness_items(reasons, readiness_reason_label)
}

fn summarized_readiness_items<T>(items: &[T], labeler: fn(&T) -> String) -> String {
    let Some(first) = items.first() else {
        return "none".to_owned();
    };
    let mut label = labeler(first);
    if items.len() > 1 {
        label.push_str(&format!("+{}", items.len() - 1));
    }
    truncate_session_view_text(&label, 48)
}

fn required_action_label(action: &sigil_kernel::RequiredAction) -> String {
    match action {
        sigil_kernel::RequiredAction::RunCheck { check_spec_id } => {
            format!("run check {check_spec_id}")
        }
        sigil_kernel::RequiredAction::ApproveCheckExecution { check_spec_id } => {
            format!("check approval {check_spec_id}")
        }
        sigil_kernel::RequiredAction::TrustWorkspace => "workspace trust required".to_owned(),
        sigil_kernel::RequiredAction::ResolveUnknownDirty => {
            "refresh source or run check".to_owned()
        }
        sigil_kernel::RequiredAction::ReRunNonWritingCheck { check_spec_id } => {
            format!("rerun non-writing check {check_spec_id}")
        }
        sigil_kernel::RequiredAction::ReviewVerificationFailure { receipt_id } => {
            format!("review verification failure {receipt_id}")
        }
        sigil_kernel::RequiredAction::ProvideVerificationConfig => {
            "verification config required".to_owned()
        }
    }
}

fn readiness_reason_label(reason: &sigil_kernel::ReadinessReason) -> String {
    match reason {
        sigil_kernel::ReadinessReason::LegacyEvidenceUnavailable => "legacy_evidence".to_owned(),
        sigil_kernel::ReadinessReason::NoVerificationRequired => {
            "no_verification_required".to_owned()
        }
        sigil_kernel::ReadinessReason::FinalAssistantTextIgnored { event_id } => {
            format!("final_text_ignored:{event_id}")
        }
        sigil_kernel::ReadinessReason::RecoveredToolError { event_id } => {
            format!("recovered_tool_error:{event_id}")
        }
        sigil_kernel::ReadinessReason::WorkspaceTrustUnsatisfied => {
            "workspace_trust_unsatisfied".to_owned()
        }
        sigil_kernel::ReadinessReason::PendingCheckReducedForTerminalRun { check_spec_id } => {
            format!("pending_terminal:{check_spec_id}")
        }
        sigil_kernel::ReadinessReason::MissingRequiredCheck { check_spec_id } => {
            format!("missing_check:{check_spec_id}")
        }
        sigil_kernel::ReadinessReason::VerificationPassed { receipt_id } => {
            format!("verification_passed:{receipt_id}")
        }
        sigil_kernel::ReadinessReason::VerificationFailed { receipt_id } => {
            format!("verification_failed:{receipt_id}")
        }
        sigil_kernel::ReadinessReason::VerificationSkipped { event_id } => {
            format!("verification_skipped:{event_id}")
        }
        sigil_kernel::ReadinessReason::VerificationStale(cause) => {
            format!(
                "verification_stale:{}",
                verification_stale_reason_label(&cause.reason)
            )
        }
        sigil_kernel::ReadinessReason::WorkspaceMutationSource {
            source_label,
            recovery_hint,
            ..
        } => recovery_hint
            .as_deref()
            .map(|hint| format!("{source_label}: {hint}"))
            .unwrap_or_else(|| source_label.clone()),
        sigil_kernel::ReadinessReason::WorkspaceUnknownDirty { event_id } => event_id
            .as_deref()
            .map(|event_id| format!("workspace_unknown_dirty:{event_id}"))
            .unwrap_or_else(|| "workspace_unknown_dirty".to_owned()),
        sigil_kernel::ReadinessReason::CheckMutatedVerificationScope { check_spec_id } => {
            format!("check_mutated_scope:{check_spec_id}")
        }
        sigil_kernel::ReadinessReason::ReceiptScopeMismatch { receipt_id } => {
            format!("receipt_scope_mismatch:{receipt_id}")
        }
        sigil_kernel::ReadinessReason::ReceiptSnapshotMismatch { receipt_id } => {
            format!("receipt_snapshot_mismatch:{receipt_id}")
        }
    }
}

fn verification_stale_reason_label(reason: &sigil_kernel::VerificationStaleReason) -> String {
    match reason {
        sigil_kernel::VerificationStaleReason::WorkspaceChanged(event_id) => {
            format!("workspace_changed:{event_id}")
        }
        sigil_kernel::VerificationStaleReason::CheckSpecChanged(event_id) => {
            format!("check_spec_changed:{event_id}")
        }
        sigil_kernel::VerificationStaleReason::PolicyChanged(event_id) => {
            format!("policy_changed:{event_id}")
        }
        sigil_kernel::VerificationStaleReason::EnvironmentChanged(event_id) => {
            format!("environment_changed:{event_id}")
        }
        sigil_kernel::VerificationStaleReason::SandboxChanged(event_id) => {
            format!("sandbox_changed:{event_id}")
        }
        sigil_kernel::VerificationStaleReason::TrustChanged(event_id) => {
            format!("trust_changed:{event_id}")
        }
        sigil_kernel::VerificationStaleReason::UnknownDirty(event_id) => {
            format!("unknown_dirty:{event_id}")
        }
    }
}

fn child_verification_link_status_label(
    entry: &sigil_kernel::ChildVerificationReceiptLinked,
) -> &'static str {
    if entry.merge_event_id.is_some() {
        "merged"
    } else {
        "linked"
    }
}

fn child_verification_parent_recheck_label(
    entry: &sigil_kernel::ChildVerificationReceiptLinked,
) -> &'static str {
    if entry.merge_event_id.is_some() {
        "required"
    } else {
        "not_required"
    }
}

fn workspace_trust_label(trust: sigil_kernel::WorkspaceTrust) -> &'static str {
    match trust {
        sigil_kernel::WorkspaceTrust::Unknown => "unknown",
        sigil_kernel::WorkspaceTrust::Trusted => "trusted",
        sigil_kernel::WorkspaceTrust::Restricted => "restricted",
        sigil_kernel::WorkspaceTrust::Denied => "denied",
    }
}

fn check_discovery_source_label(source: sigil_kernel::CheckDiscoverySource) -> &'static str {
    match source {
        sigil_kernel::CheckDiscoverySource::SigilVerificationFile => "sigil_verification_file",
        sigil_kernel::CheckDiscoverySource::UserExplicitConfig => "user_explicit_config",
        sigil_kernel::CheckDiscoverySource::CiConfig => "ci_config",
        sigil_kernel::CheckDiscoverySource::PackageScript => "package_script",
        sigil_kernel::CheckDiscoverySource::Cargo => "cargo",
        sigil_kernel::CheckDiscoverySource::Makefile => "makefile",
        sigil_kernel::CheckDiscoverySource::ModelSuggested => "model_suggested",
        sigil_kernel::CheckDiscoverySource::UserConfirmed => "user_confirmed",
    }
}

fn check_promotion_label(promotion: &sigil_kernel::CheckPromotion) -> &'static str {
    match promotion {
        sigil_kernel::CheckPromotion::UserApproved { .. } => "user_approved",
        sigil_kernel::CheckPromotion::WorkspaceTrusted { .. } => "workspace_trusted",
        sigil_kernel::CheckPromotion::Sandboxed { .. } => "sandboxed",
        sigil_kernel::CheckPromotion::GlobalPolicy { .. } => "global_policy",
        sigil_kernel::CheckPromotion::ExplicitUserConfig { .. } => "explicit_user_config",
    }
}

fn evidence_scope_label(scope: &sigil_kernel::EvidenceScope) -> String {
    match scope {
        sigil_kernel::EvidenceScope::Run(id) => format!("run:{id}"),
        sigil_kernel::EvidenceScope::Workspace(id) => format!("workspace:{id}"),
        sigil_kernel::EvidenceScope::Task(id) => format!("task:{id}"),
        sigil_kernel::EvidenceScope::Step(id) => format!("step:{id}"),
        sigil_kernel::EvidenceScope::Agent(id) => format!("agent:{id}"),
        sigil_kernel::EvidenceScope::Changeset(id) => format!("changeset:{id}"),
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

fn optional_bool_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "inherit",
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

fn agent_mailbox_status_label(status: sigil_kernel::AgentMailboxStatus) -> &'static str {
    match status {
        sigil_kernel::AgentMailboxStatus::Queued => "queued",
        sigil_kernel::AgentMailboxStatus::Delivered => "delivered",
        sigil_kernel::AgentMailboxStatus::Consumed => "consumed",
        sigil_kernel::AgentMailboxStatus::Rejected => "rejected",
        sigil_kernel::AgentMailboxStatus::Interrupted => "interrupted",
        sigil_kernel::AgentMailboxStatus::Unknown => "unknown",
    }
}

fn plugin_hook_execution_status_label(
    status: sigil_kernel::PluginHookExecutionStatus,
) -> &'static str {
    match status {
        sigil_kernel::PluginHookExecutionStatus::Succeeded => "succeeded",
        sigil_kernel::PluginHookExecutionStatus::Failed => "failed",
        sigil_kernel::PluginHookExecutionStatus::TimedOut => "timed_out",
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
