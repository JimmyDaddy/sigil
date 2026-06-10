use std::{sync::mpsc, thread};

use super::*;
use crate::provider_status::{
    BalanceSnapshot, fetch_provider_balance_snapshot, resolve_provider_api_key,
};
use crate::runner::{CompactionTrigger, WorkerCommand, WorkerMessage};
use termquill_kernel::{
    ControlEntry, EventHandler, RunEvent, ToolDiffBudget, ToolPreviewSnapshot, ToolResult,
};

impl AppState {
    pub fn poll_background_tasks(&mut self) -> bool {
        let mut dirty = false;
        let mut latest_balance = None;
        if let Some(receiver) = &self.balance_refresh_rx {
            while let Ok(snapshot) = receiver.try_recv() {
                latest_balance = Some(snapshot);
            }
        }
        if let Some(snapshot) = latest_balance {
            self.balance_snapshot = snapshot.clone();
            self.balance_refresh_rx = None;
            self.push_event("balance", snapshot.status);
            self.refresh_usage_sidebar_cache();
            dirty = true;
        }
        let mut latest_model_picker = None;
        if let Some(receiver) = &self.model_picker_refresh_rx {
            while let Ok(refresh) = receiver.try_recv() {
                latest_model_picker = Some(refresh);
            }
        }
        if let Some(refresh) = latest_model_picker {
            self.model_picker_refresh_rx = None;
            dirty |= self.apply_model_picker_refresh(refresh);
        }
        dirty
    }

    pub(super) fn schedule_balance_refresh(&mut self) {
        if self.balance_refresh_rx.is_some() || self.is_setup_mode() {
            return;
        }
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.balance_snapshot.status = "n/a".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        };
        let provider_config = termquill_runtime::resolve_deepseek_config(root_config)
            .or_else(|_| default_deepseek_provider_config(&root_config.agent.model).resolved());
        let Ok(provider_config) = provider_config else {
            self.balance_snapshot.status = "balance unavailable".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        };
        if resolve_provider_api_key(&provider_config).is_none() {
            self.balance_snapshot = BalanceSnapshot {
                status: "missing auth".to_owned(),
                ..BalanceSnapshot::default()
            };
            self.refresh_usage_sidebar_cache();
            return;
        }

        self.balance_snapshot.status = "loading".to_owned();
        self.refresh_usage_sidebar_cache();
        let (tx, rx) = mpsc::channel();
        self.balance_refresh_rx = Some(rx);
        thread::spawn(move || {
            let snapshot =
                fetch_provider_balance_snapshot(&provider_config).unwrap_or(BalanceSnapshot {
                    status: "balance unavailable".to_owned(),
                    ..BalanceSnapshot::default()
                });
            let _ = tx.send(snapshot);
        });
    }

    pub fn handle_worker_message(&mut self, message: WorkerMessage) -> Result<()> {
        match message {
            WorkerMessage::Event(event) => self.handle(*event)?,
            WorkerMessage::RunStarted { prompt } => {
                self.run_phase = RunPhase::Thinking;
                self.last_notice = Some("thinking".to_owned());
                self.push_phase_marker(format!("thinking|{}", self.model_name));
                self.push_event("run:start", prompt);
            }
            WorkerMessage::RunFinished { result, entries } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.streaming_reasoning_index = None;
                self.last_notice = Some("agent idle".to_owned());
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.recompute_compaction_status(false);
                self.schedule_balance_refresh();
                self.push_event(
                    "run:finish",
                    format!(
                        "tool_calls={} final_text_bytes={}",
                        result.tool_calls,
                        result.final_text.len()
                    ),
                );
            }
            WorkerMessage::RunCancelled {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.streaming_reasoning_index = None;
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "run cancelled; restored",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::SessionSwitched {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.streaming_reasoning_index = None;
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "restored from disk",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::SessionCompacted {
                session_log_path,
                provider_name,
                model_name,
                record,
                trigger,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                match trigger {
                    CompactionTrigger::Manual => {
                        self.restore_session_view(
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                            "Session compacted.",
                        );
                    }
                    CompactionTrigger::AutomaticHardThreshold => {
                        self.session_log_path = session_log_path;
                        self.provider_name = provider_name;
                        self.model_name = model_name;
                        self.sync_current_session_state(entries);
                        self.latest_compaction_record = Some(record.clone());
                        self.recompute_compaction_status(false);
                        self.last_notice = Some("auto compacted".to_owned());
                        self.refresh_session_history();
                        self.push_timeline(
                            TimelineRole::Notice,
                            format!(
                                "Auto-compacted: summary={} tail={}.",
                                record.compacted_message_count, record.retained_tail_message_count
                            ),
                        );
                        self.push_event(
                            "compaction",
                            format!(
                                "auto hard compacted={} tail={}",
                                record.compacted_message_count, record.retained_tail_message_count
                            ),
                        );
                        self.schedule_balance_refresh();
                    }
                }
            }
            WorkerMessage::Notice(message) => {
                self.last_notice = Some(message.clone());
                self.push_timeline(TimelineRole::Notice, message.clone());
                self.push_event("worker", message);
            }
            WorkerMessage::RunFailed(error) => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.refresh_usage_sidebar_cache();
                let summary = summarize_error(&error);
                self.last_notice = Some(summary.clone());
                self.push_timeline(TimelineRole::Notice, format!("Run failed: {summary}"));
                self.push_event("run:error", error);
            }
        }
        Ok(())
    }

    pub fn shutdown_command() -> WorkerCommand {
        WorkerCommand::Shutdown
    }

    pub fn into_worker_command(&self, action: AppAction) -> WorkerCommand {
        match action {
            AppAction::SubmitPrompt(prompt) => WorkerCommand::SubmitPrompt {
                prompt,
                reasoning_effort: self.reasoning_effort.clone(),
            },
            AppAction::ApprovalDecision { call_id, approved } => {
                WorkerCommand::ApprovalDecision { call_id, approved }
            }
            AppAction::CancelRun => WorkerCommand::CancelRun,
            AppAction::CompactNow => WorkerCommand::CompactNow,
            AppAction::CheckChangedFilesDiagnostics => WorkerCommand::CheckChangedFilesDiagnostics,
            AppAction::SwitchSession { session_log_path } => {
                WorkerCommand::SwitchSession { session_log_path }
            }
            AppAction::SetupCompleted { .. }
            | AppAction::ConfigSaved { .. }
            | AppAction::RuntimeConfigUpdated { .. } => unreachable!(
                "setup/config/runtime updates are handled before worker command conversion"
            ),
        }
    }

    fn apply_code_intelligence_tool_status(&mut self, result: &ToolResult) {
        if !result.tool_name.starts_with("code_") {
            return;
        }
        let updated_server_lines = if let Some(lines) = code_intelligence_server_lines(result) {
            for (key, line) in lines {
                self.code_intelligence_server_lines.insert(key, line);
            }
            true
        } else {
            false
        };
        if let Some(status_line) = code_diagnostics_status_line(result) {
            self.code_intelligence_status = status_line;
            self.code_intelligence_diagnostics_line = Some(code_diagnostics_sidebar_line(
                &self.code_intelligence_status,
            ));
            if let Some(summaries) = code_diagnostics_by_path(result) {
                self.code_intelligence_diagnostics_by_path = summaries;
            }
        } else if let Some(status_line) = result
            .metadata
            .details
            .get("code_intelligence")
            .and_then(|details| details.get("status_line"))
            .and_then(serde_json::Value::as_str)
        {
            self.code_intelligence_status = status_line.to_owned();
            if result.is_error() && !updated_server_lines {
                self.code_intelligence_server_lines.insert(
                    "status".to_owned(),
                    format!("status: {}", self.code_intelligence_status),
                );
            }
        } else if result.is_error() {
            self.code_intelligence_status = "degraded tool error".to_owned();
            if !updated_server_lines {
                self.code_intelligence_server_lines.insert(
                    "status".to_owned(),
                    format!("status: {}", self.code_intelligence_status),
                );
            }
        } else {
            self.code_intelligence_status = "ready".to_owned();
        }
        self.push_event("code_intelligence", self.code_intelligence_status.clone());
    }
}

fn code_intelligence_server_lines(result: &ToolResult) -> Option<Vec<(String, String)>> {
    let servers = result
        .metadata
        .details
        .get("code_intelligence")
        .and_then(|details| details.get("servers"))
        .and_then(serde_json::Value::as_array)?;
    let mut lines = Vec::new();
    for server in servers {
        let server_name = server
            .get("server")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("server");
        let status = server
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("ready");
        let languages = server
            .get("languages")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|language| !language.trim().is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let label = if languages.is_empty() {
            server_name.to_owned()
        } else {
            languages.join("/")
        };
        let line = match status {
            "ready" => format!("{label}: ready {server_name}"),
            "fallback" => format!("{label}: fallback {server_name}"),
            "installed" => format!("{label}: installed {server_name}"),
            "missing" => format!("{label}: missing {server_name}"),
            "configured" => format!("{label}: configured {server_name}"),
            "disabled" => format!("{label}: disabled {server_name}"),
            other => format!("{label}: {other}"),
        };
        lines.push((server_name.to_owned(), line));
    }
    Some(lines)
}

fn code_diagnostics_status_line(result: &ToolResult) -> Option<String> {
    if result.tool_name != "code_diagnostics" || result.is_error() {
        return None;
    }
    let content = serde_json::from_str::<serde_json::Value>(&result.content).ok()?;
    let diagnostics = content
        .get("diagnostics")
        .or_else(|| content.get("results"))?
        .as_array()?;
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic
                .get("severity")
                .and_then(serde_json::Value::as_str)
                == Some("error")
        })
        .count();
    let warnings = diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic
                .get("severity")
                .and_then(serde_json::Value::as_str)
                == Some("warning")
        })
        .count();
    if errors == 0 && warnings == 0 {
        Some("diagnostics clean".to_owned())
    } else {
        Some(format!("diagnostics {errors} errors {warnings} warnings"))
    }
}

fn code_diagnostics_by_path(
    result: &ToolResult,
) -> Option<BTreeMap<String, ApprovalDiagnosticSummary>> {
    if result.tool_name != "code_diagnostics" || result.is_error() {
        return None;
    }
    let content = serde_json::from_str::<serde_json::Value>(&result.content).ok()?;
    let mut summaries = BTreeMap::<String, ApprovalDiagnosticSummary>::new();
    if let Some(paths) = content
        .get("query")
        .and_then(|query| query.get("paths"))
        .and_then(serde_json::Value::as_array)
    {
        for path in paths.iter().filter_map(serde_json::Value::as_str) {
            summaries
                .entry(normalize_diagnostic_path(path))
                .or_default();
        }
    }

    let diagnostics = content
        .get("diagnostics")
        .or_else(|| content.get("results"))?
        .as_array()?;
    for diagnostic in diagnostics {
        let Some(path) = diagnostic
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(normalize_diagnostic_path)
        else {
            continue;
        };
        let summary = summaries.entry(path).or_default();
        match diagnostic
            .get("severity")
            .and_then(serde_json::Value::as_str)
        {
            Some("error") => summary.errors += 1,
            Some("warning") => summary.warnings += 1,
            _ => {}
        }
    }
    Some(summaries)
}

fn normalize_diagnostic_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_owned()
}

fn code_diagnostics_sidebar_line(status_line: &str) -> String {
    if status_line == "diagnostics clean" {
        return "diagnostics: clean".to_owned();
    }
    status_line
        .strip_prefix("diagnostics ")
        .map(|summary| format!("diagnostics: {summary}"))
        .unwrap_or_else(|| format!("diagnostics: {status_line}"))
}

impl EventHandler for AppState {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::TextDelta(delta) => {
                self.run_phase = RunPhase::Streaming;
                self.push_phase_marker("streaming".to_owned());
                self.append_assistant_delta(&delta);
            }
            RunEvent::ReasoningDelta(delta) => {
                self.run_phase = RunPhase::Thinking;
                self.push_phase_marker(format!("thinking|{}", self.model_name));
                self.append_reasoning_delta(&delta);
            }
            RunEvent::ToolCallStarted(call) => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.finish_streaming_assistant_entry();
                self.streaming_reasoning_index = None;
                self.push_phase_marker(format!("tool|{}", call.name));
                self.push_event("tool:start", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolCallArgsDelta { .. } => {
                if !matches!(self.run_phase, RunPhase::Tool(_)) {
                    self.run_phase = RunPhase::Tool("tool".to_owned());
                }
            }
            RunEvent::ToolCallCompleted(call) => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.finish_streaming_assistant_entry();
                self.streaming_reasoning_index = None;
                self.push_phase_marker(format!("tool|{}", call.name));
                self.push_event("tool:complete", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolApprovalRequested {
                call,
                spec,
                subjects,
                preview,
            } => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.finish_streaming_assistant_entry();
                if let Some(preview) = preview.as_ref() {
                    self.tool_preview_snapshots
                        .entry(call.id.clone())
                        .or_insert_with(|| {
                            ToolPreviewSnapshot::from_preview(
                                call.id.clone(),
                                call.name.clone(),
                                preview,
                                ToolDiffBudget::default(),
                                None,
                            )
                        });
                }
                self.pending_approval = Some(PendingApproval {
                    call: call.clone(),
                    spec,
                    subjects,
                    preview,
                });
                self.active_pane = PaneFocus::Activity;
                self.approval_scroll_back = 0;
                self.approval_metadata_collapsed = false;
                self.approval_selected_file_index = 0;
                self.approval_selected_hunk_index = 0;
                self.approval_selected_action = ApprovalAction::Deny;
                self.last_notice = Some(format!("approve {}", call.name));
                self.push_event("approval:request", format!("{} {}", call.name, call.id));
                self.push_timeline(
                    TimelineRole::Notice,
                    format!("Approve {}? Y allow, N deny.", call.name),
                );
            }
            RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            } => {
                self.run_phase = RunPhase::Thinking;
                self.pending_approval = None;
                self.active_pane = PaneFocus::Composer;
                self.push_event(
                    "approval:resolved",
                    format!(
                        "{} {}",
                        call_id,
                        if approved { "approved" } else { "denied" }
                    ),
                );
                if approved {
                    self.push_timeline(TimelineRole::Notice, format!("Approved {call_id}."));
                } else {
                    self.push_timeline(
                        TimelineRole::Notice,
                        format!(
                            "Denied {call_id}: {}",
                            reason.unwrap_or_else(|| "denied".to_owned())
                        ),
                    );
                }
            }
            RunEvent::ToolResult(result) => {
                self.run_phase = RunPhase::Tool(result.tool_name.clone());
                self.streaming_reasoning_index = None;
                self.push_phase_marker(format!("tool|{}", result.tool_name));
                let status = if result.is_error() { "error" } else { "ok" };
                self.apply_code_intelligence_tool_status(&result);
                let preview = self.tool_preview_snapshots.get(&result.call_id);
                self.push_timeline(
                    TimelineRole::Tool,
                    format_tool_result_block_redacted(&result, preview, &self.secret_redactor),
                );
                self.push_event("tool:result", format!("{} {}", result.tool_name, status));
            }
            RunEvent::Usage(usage) => {
                self.stats.apply_usage(&usage);
                self.recompute_compaction_status(true);
                self.refresh_usage_sidebar_cache();
                self.push_event(
                    "usage",
                    format!(
                        "prompt={} completion={} cache_hit={} cache_miss={}",
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        usage.cache_hit_tokens,
                        usage.cache_miss_tokens
                    ),
                );
            }
            RunEvent::Control(control) => match control {
                ControlEntry::ToolPreviewCaptured(snapshot) => {
                    self.push_event(
                        "control",
                        format!(
                            "preview {} {} files={} +{} -{}",
                            snapshot.call_id,
                            snapshot.tool_name,
                            snapshot.file_diffs.len(),
                            snapshot.original_stats.added,
                            snapshot.original_stats.removed
                        ),
                    );
                    self.tool_preview_snapshots
                        .insert(snapshot.call_id.clone(), snapshot);
                }
                other => {
                    self.push_event("control", format!("{other:?}"));
                }
            },
            RunEvent::ContinuationState(state) => {
                self.push_event("continuation", state.state_kind);
            }
            RunEvent::AssistantMessage(message) => {
                self.run_phase = RunPhase::Streaming;
                self.push_phase_marker("streaming".to_owned());
                self.finish_streaming_assistant_entry();
                self.streaming_reasoning_index = None;
                if let Some(content) = message.content
                    && !content.is_empty()
                {
                    let last_is_same = self
                        .timeline
                        .last()
                        .map(|entry| entry.role == TimelineRole::Assistant && entry.text == content)
                        .unwrap_or(false);
                    if !last_is_same {
                        self.push_timeline(TimelineRole::Assistant, content);
                    }
                }
            }
            RunEvent::Notice(note) => {
                self.last_notice = Some(note.clone());
                self.push_timeline(TimelineRole::Notice, note.clone());
                self.push_event("notice", note);
            }
        }
        Ok(())
    }
}
