use anyhow::Result;

mod agent_thread;
mod background_requests;
mod command_conversion;
mod message_labels;
mod run_event_handler;
mod run_event_helpers;
mod run_status;
mod status_sync;
mod tool_card_lifecycle;
#[cfg(test)]
use status_sync::{
    code_diagnostics_by_path, code_diagnostics_sidebar_line, code_diagnostics_status_line,
    code_intelligence_server_lines, mcp_activation_event_detail, normalize_diagnostic_path,
};
use tool_card_lifecycle::tool_card_replacement_indices;
pub(super) use tool_card_lifecycle::tool_card_replacement_key;
#[cfg(test)]
use tool_card_lifecycle::{
    agent_tool_name, wait_agent_pending_key_from_result, wait_agent_pending_key_from_tool_block,
    wait_agent_pending_replacement_indices,
};

use super::{
    ActiveTaskRuntimeStatus, AppState, ApprovalPresentationState, PaneFocus, RunPhase,
    TimelineRole,
    formatting::{format_terminal_task_block_redacted, summarize_error},
};
use crate::runner::{WorkerApprovalRouteState, WorkerCommand, WorkerMessage};
use message_labels::{
    queued_prompt_summary_noun, summarize_queued_prompt, task_run_finish_notice,
    task_run_status_label,
};
use run_event_helpers::notice_is_timeline_worthy;
#[cfg(test)]
use sigil_kernel::ToolResult;
use sigil_kernel::{ControlEntry, EventHandler};

impl AppState {
    fn timeline_has_user_prompt(&self, prompt: &str) -> bool {
        self.timeline
            .iter()
            .rev()
            .any(|entry| entry.role == TimelineRole::User && entry.text == prompt)
    }

    fn set_agent_wait_phase(&mut self, profile_id: &str) {
        self.runtime.run_phase = RunPhase::Agent(profile_id.to_owned());
        self.last_notice = Some(format!("waiting for agent @{profile_id}"));
        self.push_phase_marker(format!("agent|{profile_id}"));
    }

    pub(super) fn replace_or_push_tool_card(&mut self, rendered: String) {
        if let Some(indices) = tool_card_replacement_indices(&self.timeline, &rendered) {
            self.replace_tool_timeline_entries(&indices, rendered);
        } else {
            self.push_timeline(TimelineRole::Tool, rendered);
        }
    }

    fn replace_tool_timeline_entries(&mut self, indices: &[usize], rendered: String) {
        let Some((&keep_index, duplicate_indices)) = indices.split_first() else {
            return;
        };
        let Some(entry) = self.timeline.get_mut(keep_index) else {
            return;
        };
        entry.text = rendered;
        let mut removed_duplicate = false;
        for index in duplicate_indices.iter().rev().copied() {
            if index < self.timeline.len() {
                self.timeline.remove(index);
                removed_duplicate = true;
            }
        }
        if removed_duplicate {
            self.rebuild_timeline_projection_after_entry_removal();
        } else {
            self.refresh_replaced_tool_timeline_entry(keep_index);
        }
    }

    pub(super) fn refresh_replaced_tool_timeline_entry(&mut self, index: usize) {
        if let Some(entry) = self.timeline.get(index)
            && let Some(activity) = self.tool_activity_cache_entry(index, entry)
        {
            if let Some(cached) = self
                .timeline_state
                .tool_activity_cache
                .iter_mut()
                .find(|cached| cached.index == index)
            {
                *cached = activity;
            } else {
                self.timeline_state.tool_activity_cache.push(activity);
            }
        }
        self.rerender_timeline_entry(index);
    }

    pub fn poll_background_tasks(&mut self) -> bool {
        self.poll_setup_model_catalog()
            | self.poll_connection_inventory()
            | self.reload_active_agent_child_transcript()
            | self.poll_update_task()
    }

    pub fn has_pending_background_tasks(&self) -> bool {
        self.runtime.setup_model_catalog_rx.is_some()
            || self.runtime.connection_inventory_rx.is_some()
            || self.active_agent_child_entry().is_some()
            || self.has_pending_update_task()
    }

    #[cfg(test)]
    pub(crate) fn set_pending_model_catalog_for_test(
        &mut self,
        receiver: std::sync::mpsc::Receiver<
            Result<sigil_runtime::provider_connections::ModelCatalogResult, String>,
        >,
    ) {
        self.runtime.setup_model_catalog_rx = Some(receiver);
    }

    pub fn has_pending_worker_commands(&self) -> bool {
        !self.runtime.pending_worker_commands.is_empty()
    }

    pub fn drain_pending_worker_commands(&mut self) -> Vec<WorkerCommand> {
        std::mem::take(&mut self.runtime.pending_worker_commands)
    }

    pub(crate) fn enqueue_worker_command(&mut self, command: WorkerCommand) {
        self.runtime.pending_worker_commands.push(command);
    }

    pub(crate) fn take_worker_rebind_required(&mut self) -> bool {
        std::mem::take(&mut self.runtime.worker_rebind_required)
    }

    pub fn handle_worker_message(&mut self, message: WorkerMessage) -> Result<()> {
        match message {
            WorkerMessage::WorkerReady => {
                self.clear_pending_session_route_startup();
                self.record_started_model_route();
                self.push_event("worker", "ready");
            }
            WorkerMessage::SessionAttachmentTransferred {
                session_log_path,
                attachment,
            } => {
                self.retain_worker_session_attachment(session_log_path, attachment);
            }
            WorkerMessage::SessionRouteRecoveryRequired {
                code,
                actions,
                recovery_binding,
                retryable,
                target_session,
            } => {
                if let Some(target) = target_session {
                    self.restore_session_view(
                        target.session_log_path,
                        target.provider_name,
                        target.model_name,
                        target.entries,
                        "opened session recovery",
                    );
                }
                if !recovery_binding.is_empty() {
                    self.set_pending_session_route_recovery(code, recovery_binding);
                }
                let summary = match code {
                    sigil_kernel::PublicRouteRecoveryCode::SessionRouteConfirmationRequired => {
                        "connection target changed; review /config, then save to confirm"
                    }
                    sigil_kernel::PublicRouteRecoveryCode::SessionRouteSelectionRequired => {
                        "saved connection is unavailable; select a replacement in /config"
                    }
                    sigil_kernel::PublicRouteRecoveryCode::ModelRouteNotConfigured => {
                        "provider setup is required; open /config"
                    }
                    sigil_kernel::PublicRouteRecoveryCode::ConnectionConfigInvalid => {
                        "connection configuration is invalid; repair it in /config"
                    }
                    sigil_kernel::PublicRouteRecoveryCode::ProviderUnavailable => {
                        "provider is temporarily unavailable; retry or repair the connection"
                    }
                    sigil_kernel::PublicRouteRecoveryCode::SessionAlreadyActive => {
                        "session is open in another Sigil window; close it there, then retry /resume"
                    }
                    sigil_kernel::PublicRouteRecoveryCode::SessionWriterBusy => {
                        "session writer is busy; wait for it to finish, then retry"
                    }
                    sigil_kernel::PublicRouteRecoveryCode::SessionStreamInvalid => {
                        "session stream is invalid; start a new session or return to the library"
                    }
                };
                self.last_notice = Some(summary.to_owned());
                self.push_timeline(
                    TimelineRole::Notice,
                    format!("Session unavailable: {summary}"),
                );
                self.push_event(
                    "session:recovery",
                    format!("{code:?}; actions={actions:?}; retryable={retryable}"),
                );
            }
            WorkerMessage::ApprovalCommandReceipt(receipt) => {
                let exact_pending = self.approval.pending.as_mut().filter(|pending| {
                    pending.call.id == receipt.call_id
                        && pending.approval_request_id == receipt.approval_request_id
                });
                if let Some(pending) = exact_pending {
                    match receipt.route_state {
                        WorkerApprovalRouteState::DecisionAccepted => {
                            pending.presentation_state =
                                ApprovalPresentationState::DecisionAccepted {
                                    command_id: receipt.command_id.clone(),
                                };
                            self.active_pane = PaneFocus::Composer;
                            self.last_notice =
                                Some("approval decision accepted; resuming run".to_owned());
                        }
                        WorkerApprovalRouteState::DeliveryUncertain => {
                            pending.presentation_state =
                                ApprovalPresentationState::DeliveryUncertain {
                                    command_id: receipt.command_id.clone(),
                                };
                            self.active_pane = PaneFocus::Composer;
                            self.last_notice = Some(
                                "approval delivery is uncertain; awaiting authoritative state"
                                    .to_owned(),
                            );
                        }
                        WorkerApprovalRouteState::Rejected => {
                            pending.presentation_state = ApprovalPresentationState::Pending;
                            self.last_notice = Some(
                                "approval command was rejected; review the current request"
                                    .to_owned(),
                            );
                        }
                    }
                    self.push_event(
                        "approval:receipt",
                        format!(
                            "{} {} {:?}{}",
                            receipt.call_id,
                            receipt.approval_request_id,
                            receipt.route_state,
                            if receipt.replayed { " replayed" } else { "" }
                        ),
                    );
                } else {
                    self.push_event(
                        "approval:receipt-ignored",
                        format!(
                            "{} {} {:?}",
                            receipt.call_id, receipt.approval_request_id, receipt.route_state
                        ),
                    );
                }
            }
            WorkerMessage::Event(event) => self.handle(*event)?,
            WorkerMessage::RunStarted { prompt } => {
                self.start_worker_run_phase(
                    RunPhase::Thinking,
                    "thinking",
                    format!("thinking|{}", self.runtime.model_name),
                );
                self.push_event("run:start", sigil_kernel::safe_persistence_text(&prompt));
            }
            WorkerMessage::SkillRunStarted { skill_id, prompt } => {
                self.start_worker_run_phase(
                    RunPhase::Thinking,
                    format!("skill {skill_id} running"),
                    format!("skill|{skill_id}"),
                );
                self.push_timeline(TimelineRole::Notice, format!("skill {skill_id} started"));
                self.push_event("skill:start", sigil_kernel::safe_persistence_text(&prompt));
            }
            WorkerMessage::PlanRunStarted { prompt } => {
                self.start_worker_run_phase(
                    RunPhase::Thinking,
                    "planning",
                    format!("plan|{}", self.runtime.model_name),
                );
                self.push_event("plan:start", sigil_kernel::safe_persistence_text(&prompt));
            }
            WorkerMessage::AgentRunStarted { profile_id, prompt } => {
                self.start_worker_run_phase(
                    RunPhase::Agent(profile_id.clone()),
                    format!("waiting for agent @{profile_id}"),
                    format!("agent|{profile_id}"),
                );
                self.push_event("agent:start", sigil_kernel::safe_persistence_text(&prompt));
            }
            WorkerMessage::AgentResultContinuationStarted { thread_ids } => {
                self.start_worker_run_phase(
                    RunPhase::Thinking,
                    "agent result ready; resuming main",
                    format!("agent-result|{}", self.runtime.model_name),
                );
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
                self.resolve_deferred_queue_promotions(&items);
                self.sync_current_session_state(entries);
                let visible_target = self.active_conversation_queue_target();
                let visible_items = items
                    .iter()
                    .filter(|item| {
                        item.status == sigil_kernel::ConversationInputStatus::Queued
                            && visible_target
                                .as_ref()
                                .is_some_and(|target| item.queued.target == *target)
                    })
                    .collect::<Vec<_>>();
                let summary = if let Some(next) = visible_items.first() {
                    let noun = queued_prompt_summary_noun(&next.queued.target);
                    let plural = if visible_items.len() == 1 { "" } else { "s" };
                    format!(
                        "{} {} {noun}{plural} · next {}",
                        if paused { "paused" } else { "pending" },
                        visible_items.len(),
                        summarize_queued_prompt(&next.queued.prompt)
                    )
                } else {
                    "no follow-ups pending".to_owned()
                };
                self.last_notice = Some(summary.clone());
                self.push_event("follow-up:update", summary);
            }
            WorkerMessage::ConversationQueueDispatchStarted { queue_id, prompt } => {
                self.start_worker_run_phase(
                    RunPhase::Thinking,
                    "running follow-up",
                    format!("follow-up|{}", self.runtime.model_name),
                );
                let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
                if !self.timeline_has_user_prompt(&safe_prompt) {
                    self.push_timeline(TimelineRole::User, safe_prompt.clone());
                }
                self.push_event(
                    "follow-up:dispatch",
                    format!("{} {}", queue_id.as_str(), safe_prompt),
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
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.recompute_compaction_status(false);
                self.schedule_balance_refresh();
                let notice = format!("agent @{profile_id} finished");
                self.last_notice = Some(notice.clone());
                self.push_event("notice", notice);
                let final_text = result.final_text.trim();
                self.push_final_assistant_message_once(final_text.to_owned());
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
                self.runtime.active_task = Some(ActiveTaskRuntimeStatus {
                    task_id: task_id.clone(),
                    objective: objective.clone(),
                });
                self.refresh_conversation_queue_selection();
                self.runtime.task_provider_route_diagnostics =
                    sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default();
                self.runtime.task_completion_progress =
                    sigil_runtime::TaskCompletionProgressSnapshot::default();
                self.start_worker_run_phase(
                    RunPhase::Thinking,
                    format!("planning task {task_id}"),
                    format!("task|{}", self.runtime.model_name),
                );
                self.push_event(
                    "task:start",
                    format!(
                        "{task_id} {}",
                        sigil_kernel::safe_persistence_text(&objective)
                    ),
                );
            }
            WorkerMessage::TaskProviderRouteDiagnosticsUpdated { snapshot } => {
                self.runtime.task_provider_route_diagnostics = snapshot;
            }
            WorkerMessage::TaskCompletionProgressUpdated { snapshot } => {
                self.runtime.task_completion_progress = snapshot;
            }
            WorkerMessage::RunFinished { result, entries } => {
                self.clear_worker_run_state();
                self.finish_worker_streams();
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
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.recompute_compaction_status(false);
                self.schedule_balance_refresh();
                let plan_projection = sigil_kernel::PlanArtifactProjection::from_entries(
                    &self.session_browser.current_entries,
                );
                if let Some(draft) = plan_projection.latest_pending_plan() {
                    let current_snapshot = self.config_snapshot.as_ref().and_then(|root_config| {
                        sigil_runtime::plan_handoff_workspace_snapshot_id(
                            root_config,
                            &self.workspace_root,
                        )
                        .ok()
                        .flatten()
                    });
                    self.set_pending_plan_approval_from_draft(draft, current_snapshot.as_deref());
                }
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
            WorkerMessage::PlanRejected { entry, entries } => {
                self.runtime.is_busy = false;
                self.approval.pending = None;
                self.clear_pending_plan_approval();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.last_notice = Some(format!("plan {} rejected", entry.plan_id.as_str()));
                self.push_event("plan:rejected", entry.plan_id.as_str().to_owned());
            }
            WorkerMessage::PlanSaved { entry, entries } => {
                self.runtime.is_busy = false;
                self.approval.pending = None;
                self.clear_pending_plan_approval();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.last_notice = Some(format!("plan {} saved", entry.plan_id.as_str()));
                self.push_event("plan:saved", entry.plan_id.as_str().to_owned());
            }
            WorkerMessage::TaskCreatedFromPlan {
                entry,
                start_mode,
                entries,
            } => {
                self.clear_pending_plan_approval();
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.last_notice = Some(if entry.stale_reason.is_some() {
                    format!("task {} created from stale plan", entry.task_id.as_str())
                } else {
                    match start_mode {
                        sigil_kernel::PlanTaskStartMode::CreatePaused => {
                            format!("task {} created from plan", entry.task_id.as_str())
                        }
                        sigil_kernel::PlanTaskStartMode::CreateAndRun => {
                            format!("task {} created from plan", entry.task_id.as_str())
                        }
                    }
                });
                self.push_event(
                    "plan:task",
                    format!("{} -> {}", entry.plan_id.as_str(), entry.task_id.as_str()),
                );
            }
            WorkerMessage::TaskRunFinished {
                task_id,
                status,
                entries,
            } => {
                let notice = task_run_finish_notice(&task_id, status, &entries);
                self.clear_worker_run_state();
                self.finish_worker_streams();
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
            WorkerMessage::RunCancellationRequested => {
                self.last_notice = Some("cancelling — waiting for active work to stop".to_owned());
                self.push_event("run:cancel", "cancellation requested".to_owned());
            }
            WorkerMessage::TaskPauseRequested { task_id } => {
                self.last_notice = Some(format!(
                    "pausing task {task_id} — waiting for active work to stop"
                ));
                self.push_event("task:pause", format!("task {task_id} pause requested"));
            }
            WorkerMessage::TaskRunPaused {
                task_id,
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "task paused; restored",
                );
                self.last_notice = Some(format!(
                    "task {task_id} paused; use /task continue to resume"
                ));
                self.schedule_balance_refresh();
            }
            WorkerMessage::RunCancelled {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "run cancelled; restored",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::RunInterrupted {
                session_log_path,
                provider_name,
                model_name,
                reason,
                entries,
            } => {
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "run interrupted — cleanup could not be confirmed",
                );
                self.last_notice = Some(format!("run interrupted: {reason}"));
                self.schedule_balance_refresh();
            }
            WorkerMessage::TerminalTaskUpdated {
                identity,
                entry,
                entries,
            } => {
                self.pending_terminal_cancel_confirmation = None;
                self.sync_current_session_state(entries);
                if entry.status.is_active() {
                    self.terminal_task_control_identities
                        .insert(identity.task_id.clone(), identity);
                } else {
                    self.terminal_task_control_identities
                        .remove(entry.handle.task_id.as_str());
                }
                self.last_notice = Some(format!(
                    "terminal task {} {}",
                    entry.handle.task_id.as_str(),
                    entry.status.as_str()
                ));
                self.replace_or_push_tool_card(format_terminal_task_block_redacted(
                    &entry,
                    &self.secret_redactor,
                ));
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
            WorkerMessage::AgentThreadCancelled { thread_id, entries } => {
                self.apply_agent_thread_cancelled(thread_id, entries);
            }
            WorkerMessage::SessionSwitched {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.runtime.session_delta_stats = sigil_kernel::SessionStats::default();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "restored from disk",
                );
                self.runtime.worker_rebind_required = true;
                self.schedule_balance_refresh();
            }
            WorkerMessage::NewSessionStarted {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.runtime.session_delta_stats = sigil_kernel::SessionStats::default();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "started new session",
                );
                self.runtime.worker_rebind_required = true;
                self.schedule_balance_refresh();
            }
            WorkerMessage::V2CompactionPreviewed { state } => {
                self.apply_v2_compaction_preview(state);
            }
            WorkerMessage::V2CompactionApplied {
                request_id: _,
                source,
                compaction_id,
                folded_event_count,
                entries,
            } => {
                self.apply_v2_compaction_applied(
                    source,
                    compaction_id,
                    folded_event_count,
                    entries,
                );
            }
            WorkerMessage::StandaloneToolOutputShrinkApplied {
                request_id: _,
                context_epoch_id,
                projected_output_count,
                entries,
            } => {
                self.apply_standalone_tool_output_shrink(
                    context_epoch_id,
                    projected_output_count,
                    entries,
                );
            }
            WorkerMessage::V2CompactionApplyFailed {
                request_id: _,
                error,
            } => {
                self.apply_v2_compaction_failed(error);
            }
            WorkerMessage::CheckpointRestorePreviewed {
                request_id,
                preview,
            } => {
                self.apply_checkpoint_restore_preview(request_id, preview);
            }
            WorkerMessage::IntentStackLoaded {
                request_id,
                stack_state,
            } => {
                if !self.apply_intent_stack_loaded(request_id, stack_state) {
                    self.push_event(
                        "intent:stack",
                        format!("ignored stale load response {request_id}"),
                    );
                }
            }
            WorkerMessage::IntentDropPreviewed {
                request_id,
                preview,
            } => {
                if !self.apply_intent_drop_preview(request_id, preview) {
                    self.push_event(
                        "intent:stack",
                        format!("ignored stale Drop preview {request_id}"),
                    );
                }
            }
            WorkerMessage::IntentDropCompleted {
                request_id,
                execution,
                stack_state,
                entries,
            } => {
                if self.apply_intent_drop_completed(request_id, &execution, stack_state) {
                    self.sync_current_session_state(entries);
                    self.push_event(
                        "intent:stack",
                        format!(
                            "Drop {} resolved {:?}",
                            execution.preview.operation_id.as_str(),
                            execution.resolution
                        ),
                    );
                } else {
                    self.push_event(
                        "intent:stack",
                        format!("ignored stale Drop completion {request_id}"),
                    );
                }
            }
            WorkerMessage::IntentStackOperationFailed { request_id, error } => {
                let summary = summarize_error(&error);
                if self.apply_intent_stack_operation_failed(request_id, &summary) {
                    self.push_event("intent:stack:error", error);
                } else {
                    self.push_event(
                        "intent:stack",
                        format!("ignored stale failure response {request_id}"),
                    );
                }
            }
            WorkerMessage::TaskIntegrationReviewLoaded {
                request,
                aggregate_diff,
            } => {
                let expected = self.review.integration_review_request.as_ref() == Some(&request);
                let current = sigil_kernel::task_integration_review_product(
                    &self.session_browser.current_entries,
                )
                .is_some_and(|product| product.request == request);
                if !expected || !current {
                    self.push_event(
                        "integration:review",
                        format!("ignored stale review response {}", request.request_id),
                    );
                } else {
                    self.review.integration_review_diff_lines =
                        aggregate_diff.split('\n').map(str::to_owned).collect();
                    self.review.verification_inspect_open = true;
                    self.last_notice = Some(format!(
                        "integration diff ready for plan v{}",
                        request.plan_version
                    ));
                    self.push_event(
                        "integration:review",
                        format!("loaded exact diff {}", request.preview_digest),
                    );
                }
            }
            WorkerMessage::TaskIntegrationReviewFailed { request, error } => {
                if self.review.integration_review_request.as_ref() != Some(&request) {
                    self.push_event(
                        "integration:review",
                        format!("ignored stale review error {}", request.request_id),
                    );
                } else {
                    let current = sigil_kernel::task_integration_review_product(
                        &self.session_browser.current_entries,
                    )
                    .is_some_and(|product| product.request == request);
                    if !current {
                        self.clear_integration_review();
                    }
                    self.last_notice = Some(format!("integration review unavailable: {error}"));
                    self.push_event("integration:review", error);
                }
            }
            WorkerMessage::TaskIntegrationAccepted {
                request,
                promotion_status,
                parent_verdict,
                entries,
            } => {
                self.clear_worker_run_state();
                if self.review.integration_review_request.as_ref() != Some(&request) {
                    self.push_event(
                        "integration:accept",
                        format!("ignored stale acceptance response {}", request.request_id),
                    );
                } else {
                    let synthesis_ready = matches!(
                        parent_verdict,
                        Some(
                            sigil_kernel::VerificationVerdict::Passed
                                | sigil_kernel::VerificationVerdict::NotApplicable
                        )
                    );
                    self.sync_current_session_state(entries);
                    self.clear_integration_review();
                    let parent = parent_verdict
                        .map(|verdict| format!(" · parent {verdict:?}"))
                        .unwrap_or_default();
                    self.last_notice = Some(if synthesis_ready {
                        format!(
                            "integration {}{parent} · resuming synthesis",
                            promotion_status.as_str()
                        )
                    } else {
                        format!("integration {}{parent}", promotion_status.as_str())
                    });
                    self.push_event(
                        "integration:accept",
                        format!(
                            "{} preview={}",
                            promotion_status.as_str(),
                            request.preview_digest
                        ),
                    );
                    if synthesis_ready {
                        self.start_worker_run_phase(
                            RunPhase::Thinking,
                            "resuming task synthesis",
                            format!("task-synthesis|{}", request.task_id.as_str()),
                        );
                        self.enqueue_worker_command(WorkerCommand::ContinueTask {
                            task_id: Some(request.task_id.as_str().to_owned()),
                            guidance: None,
                        });
                    }
                }
            }
            WorkerMessage::TaskIntegrationAcceptanceFailed {
                request,
                error,
                entries,
            } => {
                self.clear_worker_run_state();
                if self.review.integration_review_request.as_ref() != Some(&request) {
                    self.push_event(
                        "integration:accept",
                        format!("ignored stale acceptance error {}", request.request_id),
                    );
                } else {
                    if !entries.is_empty() {
                        self.sync_current_session_state(entries);
                    }
                    self.last_notice = Some(format!("integration acceptance failed: {error}"));
                    self.push_event("integration:accept", error);
                }
            }
            WorkerMessage::CheckpointRestoreCompleted {
                request_id,
                preview,
                batch_id,
                entries,
            } => {
                if self.checkpoint_request_matches(request_id) {
                    self.sync_current_session_state(entries);
                    if self.apply_checkpoint_restore_completed(request_id, &preview) {
                        self.push_event("checkpoint", format!("restore batch {batch_id} applied"));
                    }
                } else {
                    self.push_event(
                        "checkpoint",
                        format!("ignored stale restore response {request_id}"),
                    );
                }
            }
            WorkerMessage::ConversationForked {
                request_id,
                session_log_path,
                provider_name,
                model_name,
                copied_message_count,
                entries,
            } => {
                if !self.checkpoint_request_matches(request_id) {
                    self.push_event(
                        "checkpoint",
                        format!("ignored stale fork response {request_id}"),
                    );
                    return Ok(());
                }
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.runtime.session_delta_stats = sigil_kernel::SessionStats::default();
                self.clear_checkpoint_interaction();
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "conversation fork created",
                );
                self.runtime.worker_rebind_required = true;
                self.last_notice = Some(format!(
                    "conversation fork created with {copied_message_count} safe message(s); workspace files are shared"
                ));
                self.push_timeline(
                    TimelineRole::Notice,
                    "Conversation fork created. Active approvals/tasks were not copied; workspace files remain shared.",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::LocalSessionInspected { request_id, entry } => {
                if !self.apply_local_session_inspected(request_id, entry) {
                    self.push_event(
                        "session:lifecycle",
                        format!("ignored stale inspect response {request_id}"),
                    );
                }
            }
            WorkerMessage::ToolArtifactPageRead {
                request_id,
                page,
                entries,
            } => {
                self.sync_current_session_state(entries);
                if self.apply_tool_artifact_page(&page) {
                    self.push_event(
                        "tool:artifact-page",
                        format!(
                            "request={request_id} ref={} bytes={} eof={} matches={}",
                            page.artifact_ref.artifact_id,
                            page.returned_bytes,
                            page.eof,
                            page.match_count
                        ),
                    );
                }
            }
            WorkerMessage::ToolArtifactPageReadFailed {
                request_id,
                artifact_ref,
                failure,
                entries,
            } => {
                if !entries.is_empty() {
                    self.sync_current_session_state(entries);
                }
                self.apply_tool_artifact_read_failure(&artifact_ref, failure);
                self.push_event(
                    "tool:artifact-page",
                    format!(
                        "request={request_id} ref={} failure={failure:?}",
                        artifact_ref.artifact_id
                    ),
                );
            }
            WorkerMessage::LocalSessionForked {
                request_id,
                session_log_path,
                provider_name,
                model_name,
                copied_message_count,
                entries,
            } => {
                if !self.local_session_action_request_matches(request_id) {
                    self.push_event(
                        "session:lifecycle",
                        format!("ignored stale fork response {request_id}"),
                    );
                    return Ok(());
                }
                self.clear_worker_run_state();
                self.finish_worker_streams();
                self.runtime.session_delta_stats = sigil_kernel::SessionStats::default();
                self.modal_state = None;
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "local conversation fork created",
                );
                self.runtime.worker_rebind_required = true;
                self.last_notice = Some(format!(
                    "conversation fork created with {copied_message_count} safe message(s); workspace files are shared"
                ));
                self.push_timeline(
                    TimelineRole::Notice,
                    "Conversation fork created. Active approvals/tasks were not copied; workspace files remain shared.",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::LocalSessionExported { request_id, output } => {
                if !self.apply_local_session_exported(request_id, &output) {
                    self.push_event(
                        "session:lifecycle",
                        format!("ignored stale export response {request_id}"),
                    );
                }
            }
            WorkerMessage::LocalSessionPinChanged { request_id, entry } => {
                if self.apply_local_session_pin_changed(request_id, entry) {
                    self.refresh_session_history();
                } else {
                    self.push_event(
                        "session:lifecycle",
                        format!("ignored stale pin response {request_id}"),
                    );
                }
            }
            WorkerMessage::LocalSessionDeletePreviewed {
                request_id,
                preview,
            } => {
                if !self.apply_local_session_delete_preview(request_id, preview) {
                    self.push_event(
                        "session:lifecycle",
                        format!("ignored stale delete preview {request_id}"),
                    );
                }
            }
            WorkerMessage::LocalSessionDeleted { request_id, output } => {
                if !self.apply_local_session_deleted(request_id, &output) {
                    self.push_event(
                        "session:lifecycle",
                        format!("ignored stale delete response {request_id}"),
                    );
                }
            }
            WorkerMessage::SessionRetentionPreviewed {
                request_id,
                preview,
            } => {
                if !self.apply_session_retention_preview(request_id, preview) {
                    self.push_event(
                        "session:retention",
                        format!("ignored stale preview response {request_id}"),
                    );
                }
            }
            WorkerMessage::SessionRetentionApplied { request_id, output } => {
                if !self.apply_session_retention_output(request_id, &output) {
                    self.push_event(
                        "session:retention",
                        format!("ignored stale apply response {request_id}"),
                    );
                }
            }
            WorkerMessage::LocalSessionLifecycleFailed { request_id, error } => {
                let summary = summarize_error(&error);
                if self.apply_local_session_lifecycle_failed(request_id, summary.clone()) {
                    self.last_notice = Some(summary);
                    self.push_event("session:lifecycle:error", error);
                } else {
                    self.push_event(
                        "session:lifecycle",
                        format!("ignored stale failure response {request_id}"),
                    );
                }
            }
            WorkerMessage::CheckpointOperationFailed { request_id, error } => {
                let summary = summarize_error(&error);
                if self.apply_checkpoint_operation_failed(request_id, &summary) {
                    self.last_notice = Some(summary.clone());
                    self.push_timeline(
                        TimelineRole::Notice,
                        format!("Checkpoint operation failed: {summary}"),
                    );
                    self.push_event("checkpoint:error", error);
                } else {
                    self.push_event(
                        "checkpoint",
                        format!("ignored stale failure response {request_id}"),
                    );
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
            WorkerMessage::McpOAuthStatus { status, revocation } => {
                self.apply_mcp_oauth_status(status, revocation);
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
            WorkerMessage::ConnectionModelsRefreshed { result } => {
                self.apply_connection_models_refresh(result);
            }
            WorkerMessage::McpElicitationRequest {
                request,
                response_tx,
            } => {
                self.open_mcp_elicitation(request, response_tx);
            }
            WorkerMessage::EgressDisclosureRequested {
                disclosure,
                receipt_tx,
            } => {
                self.open_egress_disclosure(disclosure, receipt_tx);
            }
            WorkerMessage::RunFailed(error) => {
                self.clear_checkpoint_interaction();
                self.clear_worker_run_state();
                self.discard_worker_streaming_assistant_and_finish_reasoning();
                self.refresh_usage_sidebar_cache();
                let summary = summarize_error(&error);
                self.last_notice = Some(summary.clone());
                self.push_timeline(TimelineRole::Notice, format!("Run failed: {summary}"));
                self.push_event("run:error", error);
            }
        }
        Ok(())
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/worker_bridge_detail_tests.rs"]
mod tests;
