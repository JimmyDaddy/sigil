use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use sigil_kernel::{CompactionPreview, ToolEgressEntry, ToolExecutionStatus, ToolPreviewSnapshot};
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, RootConfig, Session, SessionLogEntry,
    inspect_memory_documents, latest_compaction_record, session_stats_from_entries,
};
#[cfg(test)]
use sigil_kernel::{ModelMessage, ToolExecutionEntry};
use uuid::Uuid;

use super::{
    AppState, RunPhase, SessionHistoryEntry, SessionHistoryRow, SessionViewMode, TimelineRole,
    formatting::{
        format_agent_thread_started_block, format_agent_thread_status_block,
        format_terminal_task_block_redacted, format_tool_content_block_redacted_for_restore,
        human_file_size, relative_age_label,
    },
};
#[cfg(test)]
use crate::input::PaneFocus;
use crate::view_model::{RecoveryPanelViewModel, TaskMemoryInspectViewModel};

mod audit_log;
pub(super) use audit_log::render_control_entry_line;
mod history;
mod restore_projection;
#[cfg(test)]
use audit_log::{
    agent_invocation_mode_label, agent_route_status_label, agent_terminal_status_label,
    agent_thread_status_label, agent_trust_state_label, check_discovery_source_label,
    check_promotion_label, child_verification_link_status_label,
    child_verification_parent_recheck_label, evidence_scope_label, plan_approval_expiry_label,
    plan_approval_permission_label, readiness_reason_label, readiness_required_actions_label,
    receipt_status_label, required_action_label, run_status_label, task_child_session_status_label,
    task_plan_status_label, task_route_status_label, task_run_status_label, task_step_status_label,
    tool_approval_action_label, tool_execution_status_label, verification_check_run_status_label,
    verification_stale_reason_label, verification_verdict_label, workspace_trust_label,
};
use audit_log::{
    render_compaction_preview_lines, render_model_message_line, render_session_log_entry,
    render_tool_execution_line, restored_reasoning_note, restored_tool_call_index,
    restored_tool_execution_content, restored_tool_execution_index,
    restored_tool_preview_snapshot_index, restored_tool_result_call_ids,
    should_render_restored_tool_execution, unix_time_ms,
};
pub(super) use history::{current_focus_label, session_history_display_label, short_session_token};
#[cfg(test)]
use history::{read_bounded_line, session_history_label};
use history::{session_history_title_from_log, session_id_from_path};
#[cfg(test)]
use restore_projection::push_restored_reasoning_timeline_entry;
use restore_projection::{
    restored_timeline_entries_from_session_entries, suppressed_assistant_preamble_indices,
    suppressed_reasoning_trace_indices,
};
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

    pub fn restore_session_selector_from_disk(
        &mut self,
        selector: &str,
        fallback_provider_name: &str,
        fallback_model_name: &str,
        notice: &str,
    ) -> bool {
        self.refresh_session_history();
        let Some(session_log_path) = self.resolve_resume_target(selector) else {
            return false;
        };
        self.restore_session_path_from_disk(
            session_log_path,
            fallback_provider_name,
            fallback_model_name,
            notice,
        )
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
        self.reconcile_optimistic_conversation_queue_items();
        self.refresh_active_agent_view_after_parent_sync();
        self.refresh_conversation_queue_selection();
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
        self.reconcile_optimistic_conversation_queue_items();
        self.refresh_active_agent_view_after_parent_sync();
        self.refresh_conversation_queue_selection();
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
        self.push_event("permission_mode", self.runtime.permission_mode.clone());
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

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/session_flow_detail_tests.rs"]
mod tests;
