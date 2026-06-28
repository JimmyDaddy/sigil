use std::collections::BTreeMap;

use anyhow::Result;

use super::{
    AgentView, AppAction, AppState, ApprovalAction, ApprovalDiagnosticSummary,
    McpServerRuntimeStatus, ModelPickerRefresh, PaneFocus, PendingApproval, RunPhase,
    TimelineEntry, TimelineRole,
    formatting::{
        format_agent_thread_started_block, format_agent_thread_status_block,
        format_terminal_task_block_redacted, format_tool_result_block_redacted, summarize_error,
    },
};
use crate::config_panel::{DEEPSEEK_PROVIDER_KEY, normalize_provider_name};
use crate::runner::{CompactionTrigger, McpActivationStatus, WorkerCommand, WorkerMessage};
use sigil_kernel::{
    ControlEntry, EventHandler, RunEvent, ToolCall, ToolDiffBudget, ToolExecutionStatus,
    ToolPreviewSnapshot, ToolResult,
};
use sigil_runtime::{BalanceSnapshot, deepseek_provider_status_config};

impl AppState {
    fn set_agent_wait_phase(&mut self, profile_id: &str) {
        self.run_phase = RunPhase::Agent(profile_id.to_owned());
        self.last_notice = Some(format!("waiting for agent @{profile_id}"));
        self.push_phase_marker(format!("agent|{profile_id}"));
    }

    pub fn poll_background_tasks(&mut self) -> bool {
        self.reload_active_agent_child_transcript()
    }

    fn handle_agent_thread_event(
        &mut self,
        thread_id: &sigil_kernel::AgentThreadId,
        event: RunEvent,
    ) {
        let AgentView::Child { child_task_id, .. } = &self.active_agent_view else {
            return;
        };
        if child_task_id != thread_id.as_str() {
            return;
        }
        if self.active_agent_child_transcript.is_none() {
            self.reload_active_agent_child_transcript();
        }
        if self.append_live_agent_thread_event(event) {
            self.rerender_active_agent_child_transcript();
        }
    }

    fn append_live_agent_thread_event(&mut self, event: RunEvent) -> bool {
        match event {
            RunEvent::TextDelta(delta) => {
                self.append_live_child_delta(TimelineRole::Assistant, delta)
            }
            RunEvent::ReasoningDelta(delta) => {
                self.append_live_child_delta(TimelineRole::Thinking, delta)
            }
            RunEvent::ToolCallStarted(call) => {
                self.push_live_child_entry(TimelineRole::Tool, format!("Started {}", call.name))
            }
            RunEvent::ToolCallCompleted(call) => {
                self.push_live_child_entry(TimelineRole::Tool, format!("Completed {}", call.name))
            }
            RunEvent::ToolResult(result) => {
                self.push_live_child_entry(TimelineRole::Tool, result.content)
            }
            RunEvent::AssistantMessage(message) => {
                let Some(content) = message.content.filter(|content| !content.is_empty()) else {
                    return false;
                };
                self.replace_or_push_live_child_entry(TimelineRole::Assistant, content)
            }
            RunEvent::Notice(notice) => {
                if notice_is_timeline_worthy(&notice) {
                    self.push_live_child_entry(TimelineRole::Notice, notice)
                } else {
                    false
                }
            }
            RunEvent::ToolApprovalRequested { call, .. } => self.push_live_child_entry(
                TimelineRole::Notice,
                format!("Approve {} in child agent", call.name),
            ),
            RunEvent::ToolApprovalResolved {
                call_id, approved, ..
            } => self.push_live_child_entry(
                TimelineRole::Notice,
                format!(
                    "Approval {} for {}",
                    if approved { "allowed" } else { "denied" },
                    call_id
                ),
            ),
            RunEvent::ToolCallArgsDelta { .. }
            | RunEvent::Usage(_)
            | RunEvent::ContinuationState(_)
            | RunEvent::Control(_) => false,
        }
    }

    fn append_live_child_delta(&mut self, role: TimelineRole, delta: String) -> bool {
        let Some(transcript) = self.active_agent_child_transcript.as_mut() else {
            return false;
        };
        transcript.load_error = None;
        if let Some(entry) = transcript
            .timeline_entries
            .last_mut()
            .filter(|entry| entry.role == role)
        {
            entry.text.push_str(&delta);
        } else {
            transcript
                .timeline_entries
                .push(TimelineEntry { role, text: delta });
            transcript.total_timeline_entries = transcript
                .total_timeline_entries
                .max(transcript.timeline_entries.len());
        }
        true
    }

    fn push_live_child_entry(&mut self, role: TimelineRole, text: String) -> bool {
        let Some(transcript) = self.active_agent_child_transcript.as_mut() else {
            return false;
        };
        transcript.load_error = None;
        transcript
            .timeline_entries
            .push(TimelineEntry { role, text });
        transcript.total_timeline_entries = transcript
            .total_timeline_entries
            .max(transcript.timeline_entries.len());
        true
    }

    fn replace_or_push_live_child_entry(&mut self, role: TimelineRole, text: String) -> bool {
        let Some(transcript) = self.active_agent_child_transcript.as_mut() else {
            return false;
        };
        transcript.load_error = None;
        if let Some(entry) = transcript
            .timeline_entries
            .last_mut()
            .filter(|entry| entry.role == role)
        {
            entry.text = text;
        } else {
            transcript
                .timeline_entries
                .push(TimelineEntry { role, text });
            transcript.total_timeline_entries = transcript
                .total_timeline_entries
                .max(transcript.timeline_entries.len());
        }
        true
    }

    pub fn has_pending_worker_commands(&self) -> bool {
        !self.pending_worker_commands.is_empty()
    }

    pub fn drain_pending_worker_commands(&mut self) -> Vec<WorkerCommand> {
        std::mem::take(&mut self.pending_worker_commands)
    }

    pub(super) fn enqueue_worker_command(&mut self, command: WorkerCommand) {
        self.pending_worker_commands.push(command);
    }

    pub(super) fn next_background_request_id(&mut self) -> u64 {
        let request_id = self.next_background_request_id;
        self.next_background_request_id = self.next_background_request_id.saturating_add(1);
        request_id
    }

    pub(super) fn cancel_model_picker_refresh(&mut self) {
        if let Some(refresh) = self.active_model_picker_refresh.take() {
            self.enqueue_worker_command(WorkerCommand::CancelProviderModelsRefresh {
                request_id: refresh.request_id,
            });
        }
    }

    pub(super) fn schedule_balance_refresh(&mut self) {
        if self.active_balance_refresh_id.is_some() || self.is_setup_mode() {
            return;
        }
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.balance_snapshot.status = "n/a".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        };
        if normalize_provider_name(&root_config.agent.provider) != DEEPSEEK_PROVIDER_KEY {
            self.balance_snapshot.available = false;
            self.balance_snapshot.status = "n/a".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        }
        let provider_config = deepseek_provider_status_config(root_config);
        let Ok(provider_config) = provider_config else {
            self.balance_snapshot.status = "balance unavailable".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        };
        if provider_config.api_key.is_none() {
            self.balance_snapshot.available = false;
            self.balance_snapshot.status = "missing auth".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        }

        self.balance_snapshot.status = "loading".to_owned();
        self.refresh_usage_sidebar_cache();
        let request_id = self.next_background_request_id();
        self.active_balance_refresh_id = Some(request_id);
        self.enqueue_worker_command(WorkerCommand::RefreshProviderBalance {
            request_id,
            provider_config,
        });
    }

    fn apply_provider_balance_refresh(
        &mut self,
        request_id: u64,
        snapshot: BalanceSnapshot,
    ) -> bool {
        if self.active_balance_refresh_id != Some(request_id) {
            return false;
        }
        self.active_balance_refresh_id = None;
        self.balance_snapshot = snapshot.clone();
        self.push_event("balance", snapshot.status);
        self.refresh_usage_sidebar_cache();
        true
    }

    fn apply_provider_models_refresh(
        &mut self,
        request_id: u64,
        base_url: String,
        result: Result<Vec<String>, String>,
    ) -> bool {
        let Some(active) = self.active_model_picker_refresh.as_ref() else {
            return false;
        };
        if active.request_id != request_id {
            return false;
        }
        let active = self
            .active_model_picker_refresh
            .take()
            .expect("active refresh checked above");
        self.apply_model_picker_refresh(ModelPickerRefresh {
            target: active.target,
            current: active.current,
            base_url,
            result,
        })
    }

    pub fn handle_worker_message(&mut self, message: WorkerMessage) -> Result<()> {
        match message {
            WorkerMessage::Event(event) => self.handle(*event)?,
            WorkerMessage::RunStarted { prompt } => {
                self.is_busy = true;
                self.run_phase = RunPhase::Thinking;
                self.mcp_progress = None;
                self.last_notice = Some("thinking".to_owned());
                self.push_phase_marker(format!("thinking|{}", self.model_name));
                self.push_event("run:start", prompt);
            }
            WorkerMessage::PlanRunStarted { prompt } => {
                self.is_busy = true;
                self.run_phase = RunPhase::Thinking;
                self.mcp_progress = None;
                self.last_notice = Some("planning".to_owned());
                self.push_phase_marker(format!("plan|{}", self.model_name));
                self.push_event("plan:start", prompt);
            }
            WorkerMessage::AgentRunStarted { profile_id, prompt } => {
                self.is_busy = true;
                self.run_phase = RunPhase::Agent(profile_id.clone());
                self.mcp_progress = None;
                self.last_notice = Some(format!("waiting for agent @{profile_id}"));
                self.push_phase_marker(format!("agent|{profile_id}"));
                self.push_event("agent:start", prompt);
            }
            WorkerMessage::AgentResultContinuationStarted { thread_ids } => {
                self.is_busy = true;
                self.run_phase = RunPhase::Thinking;
                self.mcp_progress = None;
                self.last_notice = Some("agent result ready; resuming main".to_owned());
                self.push_phase_marker(format!("agent-result|{}", self.model_name));
                let threads = thread_ids
                    .iter()
                    .map(sigil_kernel::AgentThreadId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                self.push_event("agent:resume", threads);
            }
            WorkerMessage::ConversationQueueUpdated {
                items,
                paused,
                entries,
            } => {
                self.sync_current_session_state(entries);
                let summary = if let Some(next) = items.first() {
                    format!(
                        "{} {} · next {}",
                        if paused { "queue paused" } else { "queued" },
                        items.len(),
                        summarize_queued_prompt(&next.queued.prompt)
                    )
                } else {
                    "queue empty".to_owned()
                };
                self.last_notice = Some(summary.clone());
                self.push_event("queue:update", summary);
            }
            WorkerMessage::ConversationQueueDispatchStarted { queue_id, prompt } => {
                self.is_busy = true;
                self.run_phase = RunPhase::Thinking;
                self.mcp_progress = None;
                self.last_notice = Some("running queued input".to_owned());
                self.push_phase_marker(format!("queued|{}", self.model_name));
                self.push_timeline(TimelineRole::User, prompt.clone());
                self.push_event(
                    "queue:dispatch",
                    format!("{} {}", queue_id.as_str(), prompt),
                );
            }
            WorkerMessage::AgentThreadEvent { thread_id, event } => {
                self.handle_agent_thread_event(&thread_id, *event);
            }
            WorkerMessage::AgentThreadStatusLive { entry } => {
                self.push_event(
                    "agent:live-status",
                    format!("{} {:?}", entry.thread_id.as_str(), entry.status),
                );
                self.append_current_session_control(ControlEntry::AgentThreadStatusChanged(entry));
            }
            WorkerMessage::AgentRunFinished {
                profile_id,
                result,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.recompute_compaction_status(false);
                self.schedule_balance_refresh();
                let notice = format!("agent @{profile_id} finished");
                self.last_notice = Some(notice.clone());
                self.push_event("notice", notice);
                let final_text = result.final_text.trim();
                if !final_text.is_empty()
                    && !self.timeline.last().is_some_and(|entry| {
                        entry.role == TimelineRole::Assistant && entry.text == final_text
                    })
                {
                    self.push_timeline(TimelineRole::Assistant, final_text.to_owned());
                }
                self.push_event(
                    "agent:finish",
                    format!(
                        "{profile_id} tool_calls={} final_text_bytes={}",
                        result.tool_calls,
                        result.final_text.len()
                    ),
                );
            }
            WorkerMessage::TaskRunStarted { task_id, objective } => {
                self.is_busy = true;
                self.run_phase = RunPhase::Thinking;
                self.mcp_progress = None;
                self.last_notice = Some(format!("planning task {task_id}"));
                self.push_phase_marker(format!("task|{}", self.model_name));
                self.push_event("task:start", format!("{task_id} {objective}"));
            }
            WorkerMessage::RunFinished { result, entries } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
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
            WorkerMessage::PlanRunFinished { result, entries } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.recompute_compaction_status(false);
                self.schedule_balance_refresh();
                self.set_pending_plan_approval_from_text(&result.final_text);
                self.last_notice = if self.pending_plan_approval().is_some() {
                    Some("plan ready".to_owned())
                } else {
                    Some("plan finished".to_owned())
                };
                self.push_event(
                    "plan:finish",
                    format!(
                        "tool_calls={} final_text_bytes={}",
                        result.tool_calls,
                        result.final_text.len()
                    ),
                );
            }
            WorkerMessage::PlanApproved { entry, entries } => {
                self.is_busy = false;
                self.pending_approval = None;
                self.clear_pending_plan_approval();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.last_notice = Some(format!(
                    "plan approved: {}",
                    plan_approval_permission_label(entry.permission)
                ));
                self.push_event(
                    "plan:approved",
                    format!("v{} {}", entry.plan_version, entry.plan_hash),
                );
            }
            WorkerMessage::TaskRunFinished {
                task_id,
                status,
                entries,
            } => {
                let notice = task_run_finish_notice(&task_id, status, &entries);
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.last_notice = Some(notice);
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.recompute_compaction_status(false);
                self.schedule_balance_refresh();
                self.push_event(
                    "task:finish",
                    format!("{task_id} status={}", task_run_status_label(status)),
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
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "run cancelled; restored",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::TerminalTaskUpdated { entry, entries } => {
                self.pending_terminal_cancel_confirmation = None;
                self.sync_current_session_state(entries);
                self.last_notice = Some(format!(
                    "terminal task {} {}",
                    entry.handle.task_id.as_str(),
                    entry.status.as_str()
                ));
                self.push_timeline(
                    TimelineRole::Tool,
                    format_terminal_task_block_redacted(&entry, &self.secret_redactor),
                );
                self.push_event(
                    "terminal",
                    format!(
                        "{} status={}",
                        entry.handle.task_id.as_str(),
                        entry.status.as_str()
                    ),
                );
            }
            WorkerMessage::AgentThreadClosed { thread_id, entries } => {
                self.apply_agent_thread_closed(thread_id, entries);
            }
            WorkerMessage::SessionSwitched {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.session_delta_stats = sigil_kernel::SessionStats::default();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "restored from disk",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::NewSessionStarted {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.session_delta_stats = sigil_kernel::SessionStats::default();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "started new session",
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
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.finish_streaming_reasoning_entry();
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
                        self.latest_compaction_record = Some((*record).clone());
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
                if message.starts_with("mutation artifact cleanup:")
                    || message.starts_with("mutation artifact deleted:")
                {
                    self.refresh_mutation_artifact_retention_preview();
                }
                if notice_is_timeline_worthy(&message) {
                    self.push_timeline(TimelineRole::Notice, message.clone());
                }
                self.push_event("worker", message);
            }
            WorkerMessage::McpActivationStatus {
                server_name,
                status,
            } => {
                self.apply_mcp_activation_status(server_name, status);
            }
            WorkerMessage::McpProgress { notification } => {
                self.apply_mcp_progress(notification);
            }
            WorkerMessage::McpListChanged { notification } => {
                self.apply_mcp_list_changed(notification);
            }
            WorkerMessage::ProviderBalanceRefreshed {
                request_id,
                snapshot,
            } => {
                self.apply_provider_balance_refresh(request_id, snapshot);
            }
            WorkerMessage::ProviderModelsRefreshed {
                request_id,
                base_url,
                result,
            } => {
                self.apply_provider_models_refresh(request_id, base_url, result);
            }
            WorkerMessage::McpElicitationRequest {
                request,
                response_tx,
            } => {
                self.open_mcp_elicitation(request, response_tx);
            }
            WorkerMessage::RunFailed(error) => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.mcp_progress = None;
                self.pending_approval = None;
                self.modal_state = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.finish_streaming_reasoning_entry();
                self.refresh_usage_sidebar_cache();
                let summary = summarize_error(&error);
                self.last_notice = Some(summary.clone());
                self.push_timeline(TimelineRole::Notice, format!("Run failed: {summary}"));
                self.push_event("run:error", error);
            }
        }
        Ok(())
    }

    fn apply_mcp_activation_status(
        &mut self,
        server_name: Option<String>,
        status: McpActivationStatus,
    ) {
        let Some(server_name) = server_name else {
            self.push_event("mcp", mcp_activation_event_detail(None, &status));
            return;
        };
        let runtime_status = match &status {
            McpActivationStatus::Activating => McpServerRuntimeStatus::Activating,
            McpActivationStatus::Refreshing => McpServerRuntimeStatus::Refreshing,
            McpActivationStatus::Deferred => McpServerRuntimeStatus::Deferred,
            McpActivationStatus::Stale { capability } => McpServerRuntimeStatus::Stale {
                capability: capability.clone(),
            },
            McpActivationStatus::Ready { added_tools } => McpServerRuntimeStatus::Ready {
                tool_count: Some(*added_tools),
            },
            McpActivationStatus::Failed { error } => McpServerRuntimeStatus::Failed {
                message: error.clone(),
            },
        };
        self.mcp_server_statuses
            .insert(server_name.clone(), runtime_status);
        self.push_event(
            "mcp",
            mcp_activation_event_detail(Some(&server_name), &status),
        );
    }

    fn apply_mcp_progress(&mut self, notification: sigil_runtime::McpProgressNotification) {
        self.mcp_progress = Some(super::McpProgressState {
            server_name: notification.server_name.clone(),
            detail: mcp_progress_detail(&notification),
        });
    }

    fn apply_mcp_list_changed(&mut self, notification: sigil_runtime::McpListChangedNotification) {
        let server_name = notification.server_name.clone();
        let capability = notification.kind.as_str().to_owned();
        self.apply_mcp_activation_status(
            Some(server_name.clone()),
            McpActivationStatus::Stale {
                capability: capability.clone(),
            },
        );
        self.last_notice = Some(format!(
            "MCP {server_name} {capability} changed; refresh queued"
        ));
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
            AppAction::QueueConversationInput {
                prompt,
                kind,
                target,
            } => WorkerCommand::QueueConversationInput {
                prompt,
                kind,
                target,
                reasoning_effort: self.reasoning_effort.clone(),
            },
            AppAction::CancelQueuedConversationInput { queue_id } => {
                WorkerCommand::CancelQueuedConversationInput { queue_id }
            }
            AppAction::EditQueuedConversationInput { queue_id, prompt } => {
                WorkerCommand::EditQueuedConversationInput {
                    queue_id,
                    prompt,
                    reasoning_effort: self.reasoning_effort.clone(),
                }
            }
            AppAction::MoveQueuedConversationInput {
                queue_id,
                direction,
            } => WorkerCommand::MoveQueuedConversationInput {
                queue_id,
                direction,
            },
            AppAction::PromoteQueuedConversationInput { queue_id } => {
                WorkerCommand::PromoteQueuedConversationInput { queue_id }
            }
            AppAction::SendQueuedConversationInputNow { queue_id } => {
                WorkerCommand::SendQueuedConversationInputNow { queue_id }
            }
            AppAction::SetConversationQueuePaused { paused } => {
                WorkerCommand::SetConversationQueuePaused { paused }
            }
            AppAction::SubmitPlanPrompt(prompt) => WorkerCommand::SubmitPlanPrompt {
                prompt,
                reasoning_effort: self.reasoning_effort.clone(),
            },
            AppAction::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            } => WorkerCommand::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            },
            AppAction::InvokeInlineSkill {
                skill_id,
                arguments,
            } => WorkerCommand::InvokeInlineSkill {
                skill_id,
                arguments,
                reasoning_effort: self.reasoning_effort.clone(),
            },
            AppAction::InvokeChildSessionSkill {
                skill_id,
                arguments,
            } => WorkerCommand::InvokeChildSessionSkill {
                skill_id,
                arguments,
            },
            AppAction::InvokeAgentProfile {
                profile_id,
                prompt,
                parent_prompt,
            } => WorkerCommand::InvokeAgentProfile {
                profile_id,
                prompt,
                parent_prompt,
            },
            AppAction::SubmitTask(prompt) => WorkerCommand::SubmitTask { prompt },
            AppAction::ContinueTask { task_id, guidance } => {
                WorkerCommand::ContinueTask { task_id, guidance }
            }
            AppAction::ApprovalDecision { call_id, approved } => {
                WorkerCommand::ApprovalDecision { call_id, approved }
            }
            AppAction::ApprovalDecisionWithArgs { call_id, args_json } => {
                WorkerCommand::ApprovalDecisionWithArgs { call_id, args_json }
            }
            AppAction::BackgroundActiveAgent => WorkerCommand::BackgroundActiveAgent,
            AppAction::CancelRun => WorkerCommand::CancelRun,
            AppAction::CancelTerminalTask { task_id } => {
                WorkerCommand::CancelTerminalTask { task_id }
            }
            AppAction::CloseAgent { thread_id, reason } => {
                WorkerCommand::CloseAgent { thread_id, reason }
            }
            AppAction::MessageAgent { thread_id, prompt } => {
                WorkerCommand::MessageAgent { thread_id, prompt }
            }
            AppAction::CompactNow => WorkerCommand::CompactNow,
            AppAction::CheckChangedFilesDiagnostics => WorkerCommand::CheckChangedFilesDiagnostics,
            AppAction::CleanMutationArtifacts { target } => {
                WorkerCommand::CleanMutationArtifacts { target }
            }
            AppAction::DeleteMutationArtifact { artifact_id } => {
                WorkerCommand::DeleteMutationArtifact { artifact_id }
            }
            AppAction::ApproveVerificationCheck { check_spec_id } => {
                WorkerCommand::ApproveVerificationCheck { check_spec_id }
            }
            AppAction::SandboxVerificationCheck { check_spec_id } => {
                WorkerCommand::SandboxVerificationCheck { check_spec_id }
            }
            AppAction::ActivateLazyMcp { server_name } => {
                WorkerCommand::ActivateLazyMcp { server_name }
            }
            AppAction::RefreshMcpServer { server_name } => {
                WorkerCommand::RefreshMcpServer { server_name }
            }
            AppAction::StartNewSession { session_log_path } => {
                WorkerCommand::StartNewSession { session_log_path }
            }
            AppAction::SwitchSession { session_log_path } => {
                WorkerCommand::SwitchSession { session_log_path }
            }
            AppAction::SetupCompleted { .. }
            | AppAction::TrustWorkspace
            | AppAction::ConfigSaved { .. }
            | AppAction::RuntimeConfigUpdated { .. }
            | AppAction::CopyToClipboard { .. } => unreachable!(
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

    fn apply_mcp_activation_tool_status(&mut self, result: &ToolResult) {
        if result.tool_name != "mcp_activate_server" || result.is_error() {
            return;
        }
        let Ok(content) = serde_json::from_str::<serde_json::Value>(&result.content) else {
            return;
        };
        let Some(server_name) = content
            .get("server_name")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let status = content
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("ready");
        if status != "ready" && status != "already_ready" {
            return;
        }
        let added_tools = content
            .get("added_tools")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        self.apply_mcp_activation_status(
            Some(server_name.to_owned()),
            McpActivationStatus::Ready { added_tools },
        );
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

fn task_run_finish_notice(
    task_id: &str,
    status: sigil_kernel::TaskRunStatus,
    entries: &[sigil_kernel::SessionLogEntry],
) -> String {
    let label = task_run_status_label(status);
    let reason = entries.iter().rev().find_map(|entry| {
        let sigil_kernel::SessionLogEntry::Control(ControlEntry::TaskRun(run)) = entry else {
            return None;
        };
        if run.task_id.as_str() == task_id
            && run.status == status
            && !matches!(status, sigil_kernel::TaskRunStatus::Completed)
        {
            return run
                .reason
                .as_deref()
                .filter(|value| !value.trim().is_empty());
        }
        None
    });
    if let Some(reason) = reason {
        format!("task {task_id} {label}: {reason}")
    } else {
        format!("task {task_id} {label}")
    }
}

fn mcp_activation_event_detail(server_name: Option<&str>, status: &McpActivationStatus) -> String {
    let scope = server_name
        .map(|name| format!("server={name} "))
        .unwrap_or_default();
    let status = match status {
        McpActivationStatus::Activating => "activating".to_owned(),
        McpActivationStatus::Refreshing => "refreshing".to_owned(),
        McpActivationStatus::Deferred => "deferred".to_owned(),
        McpActivationStatus::Stale { capability } => format!("stale {capability}"),
        McpActivationStatus::Ready { added_tools } => format!("ready tools={added_tools}"),
        McpActivationStatus::Failed { error } => format!("failed {}", summarize_error(error)),
    };
    format!("{scope}{status}")
}

fn mcp_progress_detail(notification: &sigil_runtime::McpProgressNotification) -> String {
    let message = notification
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("working");
    match (notification.progress, notification.total) {
        (Some(progress), Some(total)) if total > 0.0 => format!(
            "{}: {} {:.0}%",
            notification.server_name,
            message,
            (progress / total * 100.0).clamp(0.0, 100.0)
        ),
        (Some(progress), _) => format!("{}: {} {:.0}", notification.server_name, message, progress),
        _ => format!("{}: {}", notification.server_name, message),
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
                if agent_tool_name(&call.name) {
                    self.downgrade_streaming_assistant_entry_to_thinking();
                }
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.push_phase_marker(format!("tool|{}", call.name));
                self.push_event("tool:start", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolCallArgsDelta { .. } => {
                if !matches!(self.run_phase, RunPhase::Tool(_)) {
                    self.run_phase = RunPhase::Tool("tool".to_owned());
                }
            }
            RunEvent::ToolCallCompleted(call) => {
                if agent_tool_name(&call.name) {
                    self.downgrade_streaming_assistant_entry_to_thinking();
                }
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                if let Some(profile_id) = spawn_agent_profile_id(&call) {
                    self.set_agent_wait_phase(&profile_id);
                } else {
                    self.run_phase = RunPhase::Tool(call.name.clone());
                    self.push_phase_marker(format!("tool|{}", call.name));
                }
                self.push_event("tool:complete", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolApprovalRequested {
                call,
                spec,
                subjects,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                preview,
            } => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
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
                    operation,
                    risk,
                    subject_zones,
                    confirmation,
                    snapshot_required,
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
                let approved_agent_profile = approved.then(|| {
                    self.pending_approval
                        .as_ref()
                        .and_then(|pending| spawn_agent_profile_id(&pending.call))
                });
                self.pending_approval = None;
                self.active_pane = PaneFocus::Composer;
                if let Some(Some(profile_id)) = approved_agent_profile {
                    self.set_agent_wait_phase(&profile_id);
                } else {
                    self.run_phase = RunPhase::Thinking;
                    self.push_phase_marker(format!("thinking|{}", self.model_name));
                }
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
                let is_agent_tool = agent_tool_name(&result.tool_name);
                if !is_agent_tool {
                    self.run_phase = RunPhase::Tool(result.tool_name.clone());
                }
                self.finish_streaming_reasoning_entry();
                if is_agent_tool {
                    self.run_phase = RunPhase::Thinking;
                    self.push_phase_marker(format!("thinking|{}", self.model_name));
                } else {
                    self.push_phase_marker(format!("tool|{}", result.tool_name));
                }
                let status = if result.is_error() { "error" } else { "ok" };
                self.apply_code_intelligence_tool_status(&result);
                self.apply_mcp_activation_tool_status(&result);
                let preview = self.tool_preview_snapshots.get(&result.call_id);
                let rendered =
                    format_tool_result_block_redacted(&result, preview, &self.secret_redactor);
                if should_replace_last_wait_agent_pending(&self.timeline, &result, &rendered) {
                    if let Some(entry) = self.timeline.last_mut() {
                        entry.text = rendered;
                    }
                } else {
                    self.push_timeline(TimelineRole::Tool, rendered);
                }
                self.push_event("tool:result", format!("{} {}", result.tool_name, status));
            }
            RunEvent::Usage(usage) => {
                self.stats.apply_usage(&usage);
                self.session_delta_stats.apply_usage(&usage);
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
                    let control = ControlEntry::ToolPreviewCaptured(snapshot.clone());
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
                    self.append_current_session_control(control);
                }
                ControlEntry::TerminalTask(task) => {
                    self.push_event(
                        "terminal",
                        format!(
                            "{} status={}",
                            task.handle.task_id.as_str(),
                            task.status.as_str()
                        ),
                    );
                    self.push_timeline(
                        TimelineRole::Tool,
                        format_terminal_task_block_redacted(&task, &self.secret_redactor),
                    );
                    self.append_current_session_control(ControlEntry::TerminalTask(task));
                }
                ControlEntry::ToolExecution(execution) => {
                    if matches!(execution.status, ToolExecutionStatus::Started) {
                        self.run_phase = RunPhase::Tool(execution.tool_name.clone());
                        self.push_phase_marker(format!("tool|{}", execution.tool_name));
                    }
                    let control = ControlEntry::ToolExecution(execution);
                    self.push_event("control", format!("{control:?}"));
                    self.append_current_session_control(control);
                }
                ControlEntry::AgentThreadStarted(entry) => {
                    let control = ControlEntry::AgentThreadStarted(entry.clone());
                    if matches!(
                        entry.invocation_source,
                        sigil_kernel::AgentInvocationSource::Chat
                            | sigil_kernel::AgentInvocationSource::Mention
                    ) {
                        let profile_id = entry.profile_id.as_str();
                        self.set_agent_wait_phase(profile_id);
                        self.push_timeline(
                            TimelineRole::Tool,
                            format_agent_thread_started_block(&entry),
                        );
                        self.push_event("agent:start", entry.objective.clone());
                    } else {
                        self.push_event("control", format!("{control:?}"));
                    }
                    self.append_current_session_control(control);
                }
                ControlEntry::AgentThreadStatusChanged(entry) => {
                    self.push_event(
                        "agent:status",
                        format!("{} {:?}", entry.thread_id.as_str(), entry.status),
                    );
                    self.push_timeline(
                        TimelineRole::Tool,
                        format_agent_thread_status_block(&entry),
                    );
                    self.append_current_session_control(ControlEntry::AgentThreadStatusChanged(
                        entry,
                    ));
                }
                other => {
                    self.push_event("control", format!("{other:?}"));
                    self.append_current_session_control(other);
                }
            },
            RunEvent::ContinuationState(state) => {
                self.push_event("continuation", state.state_kind);
            }
            RunEvent::AssistantMessage(message) => {
                if let Some(tool_name) = message.tool_calls.first().map(|call| call.name.clone()) {
                    self.run_phase = RunPhase::Tool(tool_name.clone());
                    self.push_phase_marker(format!("tool|{tool_name}"));
                } else {
                    self.run_phase = RunPhase::Streaming;
                    self.push_phase_marker("streaming".to_owned());
                }
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                if let Some(content) = message.content {
                    self.push_assistant_message_once(content);
                }
            }
            RunEvent::Notice(note) => {
                self.last_notice = Some(note.clone());
                if notice_is_timeline_worthy(&note) {
                    self.push_timeline(TimelineRole::Notice, note.clone());
                }
                self.push_event("notice", note);
            }
        }
        Ok(())
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

fn agent_tool_name(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent" | "wait_agent" | "read_agent_result" | "message_agent" | "close_agent"
    )
}

fn should_replace_last_wait_agent_pending(
    timeline: &[TimelineEntry],
    result: &ToolResult,
    rendered: &str,
) -> bool {
    let Some(current_key) = wait_agent_pending_key_from_result(result, rendered) else {
        return false;
    };
    let Some(previous) = timeline.last() else {
        return false;
    };
    previous.role == TimelineRole::Tool
        && wait_agent_pending_key_from_tool_block(&previous.text)
            .is_some_and(|previous_key| previous_key == current_key)
}

fn wait_agent_pending_key_from_result(result: &ToolResult, rendered: &str) -> Option<String> {
    if result.tool_name != "wait_agent" || result.is_error() {
        return None;
    }
    wait_agent_pending_key_from_tool_block(rendered)
}

fn wait_agent_pending_key_from_tool_block(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    if value.get("tool_name")?.as_str()? != "wait_agent" {
        return None;
    }
    if value.get("status").and_then(serde_json::Value::as_str) != Some("ok") {
        return None;
    }
    let preview = value.get("preview_value")?;
    if preview
        .get("terminal")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    preview.get("retry_after_ms")?;
    preview
        .get("coalescing_key")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            preview
                .get("thread_id")
                .and_then(serde_json::Value::as_str)
                .map(|thread_id| format!("wait_agent:{thread_id}"))
        })
}

fn notice_is_timeline_worthy(note: &str) -> bool {
    let normalized = note.to_ascii_lowercase();
    [
        "failed",
        "failure",
        "error",
        "denied",
        "timeout",
        "timed out",
        "deadline",
        "exceeded",
        "unavailable",
        "invalid",
        "cancelled",
        "canceled",
        "interrupted",
        "panic",
        "rejected",
        "budget",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn spawn_agent_profile_id(call: &ToolCall) -> Option<String> {
    if call.name != "spawn_agent" {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&call.args_json)
        .ok()?
        .get("profile_id")?
        .as_str()
        .filter(|profile_id| !profile_id.is_empty())
        .map(ToOwned::to_owned)
}

fn summarize_queued_prompt(prompt: &str) -> String {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 48 {
        normalized
    } else {
        format!("{}...", normalized.chars().take(45).collect::<String>())
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/worker_bridge_detail_tests.rs"]
mod tests;
