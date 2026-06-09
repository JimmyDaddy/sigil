use std::{sync::mpsc, thread};

use super::*;
use crate::provider_status::{
    BalanceSnapshot, fetch_provider_balance_snapshot, resolve_provider_api_key,
};
use crate::runner::{CompactionTrigger, WorkerCommand, WorkerMessage};
use termquill_kernel::{ControlEntry, EventHandler, RunEvent, ToolDiffBudget, ToolPreviewSnapshot};

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
        let provider_config = load_deepseek_provider_config(root_config)
            .unwrap_or_else(|| default_deepseek_provider_config(&root_config.agent.model));
        let Ok(provider_config) = provider_config.resolved() else {
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
                self.streaming_assistant_index = None;
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
                self.streaming_assistant_index = None;
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
                self.streaming_assistant_index = None;
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
}

impl EventHandler for AppState {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::TextDelta(delta) => {
                self.run_phase = RunPhase::Streaming;
                self.push_phase_marker("streaming".to_owned());
                self.append_assistant_delta(&delta);
                self.push_event("text", delta);
            }
            RunEvent::ReasoningDelta(delta) => {
                self.run_phase = RunPhase::Thinking;
                self.push_phase_marker(format!("thinking|{}", self.model_name));
                self.append_reasoning_delta(&delta);
                self.push_event("reasoning", delta);
            }
            RunEvent::ToolCallStarted(call) => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.streaming_reasoning_index = None;
                self.push_phase_marker(format!("tool|{}", call.name));
                self.push_event("tool:start", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolCallArgsDelta { id, delta } => {
                if !matches!(self.run_phase, RunPhase::Tool(_)) {
                    self.run_phase = RunPhase::Tool("tool".to_owned());
                }
                self.push_event("tool:args", format!("{id} {delta}"));
            }
            RunEvent::ToolCallCompleted(call) => {
                self.run_phase = RunPhase::Tool(call.name.clone());
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
                let preview = self.tool_preview_snapshots.get(&result.call_id);
                self.push_timeline(
                    TimelineRole::Tool,
                    format_tool_result_block(&result, preview),
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
                self.streaming_assistant_index = None;
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
