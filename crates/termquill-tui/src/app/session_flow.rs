use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use termquill_kernel::{
    CompactionPreview, ControlEntry, JsonlSessionStore, ModelMessage, RootConfig, Session,
    SessionLogEntry, ToolEgressEntry, ToolExecutionEntry, ToolExecutionStatus, ToolPreviewSnapshot,
    inspect_memory_documents, latest_compaction_record, session_stats_from_entries,
};
use uuid::Uuid;

use super::*;

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
        self.sync_current_session_state(entries.clone());
        self.pending_approval = None;
        self.run_phase = RunPhase::Idle;
        self.refresh_memory_summary();
        self.recompute_compaction_status(false);
        self.timeline.clear();
        self.tool_activity_cache.clear();
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
        termquill_kernel::MessageRole::System => "system",
        termquill_kernel::MessageRole::User => "user",
        termquill_kernel::MessageRole::Assistant => "assistant",
        termquill_kernel::MessageRole::Tool => "tool",
    };
    if !message.tool_calls.is_empty() {
        let names = message
            .tool_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return format!("[{role}] tool_calls [{names}]");
    }

    let content = truncate_session_view_text(message.content.as_deref().unwrap_or_default(), 160);
    if matches!(message.role, termquill_kernel::MessageRole::Tool) {
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
            ControlEntry::ToolPreviewCaptured(snapshot) => format!(
                "[ctl] preview {} {} files={} +{} -{}",
                snapshot.call_id,
                snapshot.tool_name,
                snapshot.file_diffs.len(),
                snapshot.original_stats.added,
                snapshot.original_stats.removed
            ),
            ControlEntry::CompactionApplied(record) => format!(
                "[ctl] compacted={} tail={}",
                record.compacted_message_count, record.retained_tail_message_count
            ),
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

fn tool_approval_action_label(action: termquill_kernel::ToolApprovalAuditAction) -> &'static str {
    match action {
        termquill_kernel::ToolApprovalAuditAction::PolicyEvaluated => "policy",
        termquill_kernel::ToolApprovalAuditAction::Requested => "requested",
        termquill_kernel::ToolApprovalAuditAction::Resolved => "resolved",
        termquill_kernel::ToolApprovalAuditAction::PreviewFailed => "preview_failed",
    }
}

fn tool_execution_status_label(status: termquill_kernel::ToolExecutionStatus) -> &'static str {
    match status {
        termquill_kernel::ToolExecutionStatus::Started => "started",
        termquill_kernel::ToolExecutionStatus::Completed => "completed",
        termquill_kernel::ToolExecutionStatus::Failed => "failed",
        termquill_kernel::ToolExecutionStatus::Cancelled => "cancelled",
        termquill_kernel::ToolExecutionStatus::Interrupted => "interrupted",
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
