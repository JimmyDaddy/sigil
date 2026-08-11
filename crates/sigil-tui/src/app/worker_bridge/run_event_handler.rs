use anyhow::Result;
use sigil_kernel::{
    ControlEntry, EventHandler, RunEvent, ToolDiffBudget, ToolExecutionStatus, ToolPreviewSnapshot,
};

use super::super::{
    AppState, ApprovalAction, PaneFocus, PendingApproval, RunPhase, TimelineRole,
    approval_flow::approval_activity_label,
    formatting::{
        format_agent_thread_started_block, format_agent_thread_status_block,
        format_terminal_task_block_redacted, format_tool_progress_block_redacted_with_call,
        format_tool_result_block_redacted_with_call,
    },
    session_flow::render_control_entry_line,
};
use super::{
    run_event_helpers::{
        notice_is_timeline_worthy, notice_rejects_current_final_candidate, spawn_agent_profile_id,
    },
    tool_card_lifecycle::{
        agent_tool_name, attach_progress_execution_id, suppress_reasoning_before_tool_call,
        tool_card_replacement_indices, tool_progress_result, tracked_tool_card_replacement_index,
        wait_agent_pending_replacement_indices,
    },
};

impl EventHandler for AppState {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::TextDelta(delta) => {
                self.runtime.run_phase = RunPhase::Streaming;
                self.push_phase_marker("streaming".to_owned());
                self.append_assistant_delta(&delta);
            }
            RunEvent::ReasoningDelta(delta) => {
                self.runtime.run_phase = RunPhase::Thinking;
                self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
                self.append_reasoning_delta(&delta);
            }
            RunEvent::ToolCallStarted(call) => {
                self.runtime.run_phase = RunPhase::Tool(call.name.clone());
                self.downgrade_streaming_assistant_entry_to_thinking();
                self.finish_streaming_assistant_entry();
                if suppress_reasoning_before_tool_call(&call.name) {
                    self.discard_streaming_reasoning_entry();
                } else {
                    self.finish_streaming_reasoning_entry();
                }
                self.push_phase_marker(format!("tool|{}", call.name));
                self.push_event("tool:start", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolCallArgsDelta { .. } => {
                if !matches!(self.runtime.run_phase, RunPhase::Tool(_)) {
                    self.runtime.run_phase = RunPhase::Tool("tool".to_owned());
                }
            }
            RunEvent::ToolCallCompleted(call) => {
                self.safe_tool_calls.insert(call.id.clone(), call.clone());
                self.downgrade_streaming_assistant_entry_to_thinking();
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                if let Some(profile_id) = spawn_agent_profile_id(&call) {
                    self.set_agent_wait_phase(&profile_id);
                } else {
                    self.runtime.run_phase = RunPhase::Tool(call.name.clone());
                    self.push_phase_marker(format!("tool|{}", call.name));
                }
                self.push_event("tool:complete", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolApprovalRequested {
                approval_identity,
                effects,
                analysis,
                containment,
                safe_summary,
                decision_reasons,
                session_grant_available,
                session_grant_unavailable_reason,
                call,
                spec,
                subjects,
                network_effect,
                local_policy_decision,
                network_policy_decision,
                source_policy_decision,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                command_permission_matches,
                preview,
                ..
            } => {
                self.runtime.run_phase = RunPhase::Tool(call.name.clone());
                self.downgrade_streaming_assistant_entry_to_thinking();
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
                let pending = PendingApproval {
                    approval_request_id: approval_identity.approval_request_id,
                    call: call.clone(),
                    session_grant_available,
                    session_grant_unavailable_reason,
                    spec,
                    effects,
                    subjects,
                    analysis,
                    containment,
                    safe_summary,
                    decision_reasons,
                    network_effect,
                    local_policy_decision,
                    network_policy_decision,
                    source_policy_decision,
                    operation,
                    risk,
                    subject_zones,
                    confirmation,
                    snapshot_required,
                    command_permission_matches,
                    command_family_allow_pattern: session_grant_available
                        .then(|| crate::approval::command_family_pattern_for_call(&call))
                        .flatten(),
                    preview,
                    presentation_state: super::super::ApprovalPresentationState::Pending,
                };
                let family_available = pending.command_family_allow_pattern.is_some();
                let activity_label = approval_activity_label(&pending);
                self.approval.pending = Some(pending);
                self.active_pane = PaneFocus::Activity;
                self.approval.scroll_back = 0;
                self.approval.metadata_collapsed = true;
                self.approval.selected_file_index = 0;
                self.approval.selected_hunk_index = 0;
                self.approval.selected_action =
                    ApprovalAction::default_for(risk, session_grant_available, family_available);
                self.last_notice = Some(format!("approve {}", call.name));
                self.push_event("approval:request", format!("{} {}", call.name, call.id));
                self.push_timeline(
                    TimelineRole::Notice,
                    format!(
                        "Approval needed · {} · Y allow once, N deny.",
                        activity_label
                    ),
                );
            }
            RunEvent::ToolApprovalResolved {
                call_id,
                approval_request_id,
                approved,
                reason,
            } => {
                if self.approval.pending.as_ref().is_some_and(|pending| {
                    pending.call.id != call_id || pending.approval_request_id != approval_request_id
                }) {
                    self.push_event(
                        "approval:resolved-ignored",
                        format!("{call_id} {approval_request_id}"),
                    );
                    return Ok(());
                }
                let resolved_presentation = self.approval.pending.as_ref().map(|pending| {
                    (
                        approval_activity_label(pending),
                        self.approval.selected_action.normalized(
                            pending.session_grant_available,
                            pending.command_family_allow_pattern.is_some(),
                        ),
                    )
                });
                let approved_agent_profile = approved.then(|| {
                    self.approval
                        .pending
                        .as_ref()
                        .and_then(|pending| spawn_agent_profile_id(&pending.call))
                });
                self.approval.pending = None;
                self.active_pane = PaneFocus::Composer;
                if let Some(Some(profile_id)) = approved_agent_profile {
                    self.set_agent_wait_phase(&profile_id);
                } else {
                    self.runtime.run_phase = RunPhase::Thinking;
                    self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
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
                    let (activity, action) = resolved_presentation
                        .unwrap_or_else(|| ("tool action".to_owned(), ApprovalAction::AllowOnce));
                    let outcome = if action == ApprovalAction::AllowSession {
                        "Allowed for session"
                    } else {
                        "Allowed once"
                    };
                    self.push_timeline(TimelineRole::Notice, format!("{outcome} · {activity}"));
                } else {
                    let activity = resolved_presentation
                        .map(|(activity, _)| activity)
                        .unwrap_or_else(|| "tool action".to_owned());
                    self.push_timeline(
                        TimelineRole::Notice,
                        format!(
                            "Denied · {activity} · {}",
                            reason.unwrap_or_else(|| "denied".to_owned())
                        ),
                    );
                }
            }
            RunEvent::ToolProgress(progress) => {
                self.runtime.run_phase = RunPhase::Tool(progress.tool_name.clone());
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.push_phase_marker(format!("tool|{}", progress.tool_name));
                let execution_id = progress.execution_id.as_str().to_owned();
                self.tool_progress_execution_ids
                    .insert(progress.call_id.clone(), execution_id.clone());
                let result = tool_progress_result(progress);
                let tool_call = self.safe_tool_calls.get(&result.call_id);
                let rendered = format_tool_progress_block_redacted_with_call(
                    &result,
                    tool_call,
                    &self.secret_redactor,
                );
                let tracked_indices = self
                    .tool_progress_entry_indices
                    .get(&execution_id)
                    .and_then(|entry_index| {
                        tracked_tool_card_replacement_index(&self.timeline, &rendered, *entry_index)
                    });
                let replacement_indices = tracked_indices
                    .or_else(|| tool_card_replacement_indices(&self.timeline, &rendered));
                let entry_index = if let Some(indices) = replacement_indices {
                    let entry_index = indices[0];
                    self.replace_tool_timeline_entries(&indices, rendered);
                    entry_index
                } else {
                    let entry_index = self.timeline.len();
                    self.push_timeline(TimelineRole::Tool, rendered);
                    entry_index
                };
                self.tool_progress_entry_indices
                    .insert(execution_id, entry_index);
                self.push_event(
                    "tool:progress",
                    format!("{} {}", result.tool_name, result.content),
                );
            }
            RunEvent::ToolResult(mut result) => {
                let refresh_workspace_git = result.tool_name == "bash"
                    || result.tool_name.starts_with("terminal_")
                    || !result.metadata.changed_files.is_empty();
                self.clear_recent_egress_disclosure();
                let is_agent_tool = agent_tool_name(&result.tool_name);
                if !is_agent_tool {
                    self.runtime.run_phase = RunPhase::Tool(result.tool_name.clone());
                }
                self.finish_streaming_reasoning_entry();
                if is_agent_tool {
                    self.runtime.run_phase = RunPhase::Thinking;
                    self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
                } else {
                    self.push_phase_marker(format!("tool|{}", result.tool_name));
                }
                let status = if result.is_error() { "error" } else { "ok" };
                self.apply_code_intelligence_tool_status(&result);
                self.apply_mcp_activation_tool_status(&result);
                let progress_execution_id = self
                    .tool_progress_execution_ids
                    .remove(result.call_id.as_str());
                if let Some(execution_id) = progress_execution_id.as_deref() {
                    attach_progress_execution_id(&mut result, execution_id);
                }
                let preview = self.tool_preview_snapshots.get(&result.call_id);
                let tool_call = self.safe_tool_calls.get(&result.call_id).cloned();
                let rendered = format_tool_result_block_redacted_with_call(
                    &result,
                    tool_call.as_ref(),
                    preview,
                    &self.secret_redactor,
                );
                self.safe_tool_calls.remove(result.call_id.as_str());
                let tracked_indices = progress_execution_id.as_deref().and_then(|execution_id| {
                    let entry_index = self.tool_progress_entry_indices.remove(execution_id)?;
                    tracked_tool_card_replacement_index(&self.timeline, &rendered, entry_index)
                });
                if let Some(indices) =
                    wait_agent_pending_replacement_indices(&self.timeline, &result, &rendered)
                {
                    self.replace_tool_timeline_entries(&indices, rendered);
                } else if let Some(indices) = tracked_indices {
                    self.replace_tool_timeline_entries(&indices, rendered);
                } else if let Some(indices) =
                    tool_card_replacement_indices(&self.timeline, &rendered)
                {
                    self.replace_tool_timeline_entries(&indices, rendered);
                } else {
                    self.push_timeline(TimelineRole::Tool, rendered);
                }
                self.push_event("tool:result", format!("{} {}", result.tool_name, status));
                if refresh_workspace_git {
                    self.refresh_workspace_git_status();
                }
            }
            RunEvent::Usage(usage) => {
                self.runtime.stats.apply_usage(&usage);
                self.runtime.session_delta_stats.apply_usage(&usage);
                self.recompute_compaction_status(true);
                self.refresh_usage_sidebar_cache();
                let write = usage
                    .cache_usage
                    .as_ref()
                    .and_then(|cache| cache.write.as_ref())
                    .map_or_else(|| "-".to_owned(), |count| count.tokens.to_string());
                let mutation = usage
                    .cache_usage
                    .as_ref()
                    .and_then(|cache| cache.local_layout_mutation)
                    .map_or("unknown", sigil_kernel::CacheLayoutMutationKind::as_str);
                let provider_miss_without_local_mutation = usage
                    .cache_usage
                    .as_ref()
                    .is_some_and(|cache| cache.provider_miss_without_local_mutation);
                self.push_event(
                    "usage",
                    format!(
                        "prompt={} completion={} cache_read={} cache_write={write} cache_miss={} layout={mutation} provider_miss_without_local_mutation={provider_miss_without_local_mutation}",
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
                    self.replace_or_push_tool_card(format_terminal_task_block_redacted(
                        &task,
                        &self.secret_redactor,
                    ));
                    self.append_current_session_control(ControlEntry::TerminalTask(task));
                    self.refresh_workspace_git_status();
                }
                ControlEntry::ToolExecution(execution) => {
                    if matches!(execution.status, ToolExecutionStatus::Started) {
                        self.runtime.run_phase = RunPhase::Tool(execution.tool_name.clone());
                        self.push_phase_marker(format!("tool|{}", execution.tool_name));
                    }
                    let control = ControlEntry::ToolExecution(execution);
                    self.push_event("control", render_control_entry_line(&control));
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
                        self.replace_or_push_tool_card(format_agent_thread_started_block(&entry));
                        self.push_event("agent:start", entry.objective.clone());
                    } else {
                        self.push_event("control", render_control_entry_line(&control));
                    }
                    self.append_current_session_control(control);
                }
                ControlEntry::AgentThreadStatusChanged(entry) => {
                    self.push_event(
                        "agent:status",
                        format!("{} {:?}", entry.thread_id.as_str(), entry.status),
                    );
                    self.replace_or_push_tool_card(format_agent_thread_status_block(&entry));
                    self.append_current_session_control(ControlEntry::AgentThreadStatusChanged(
                        entry,
                    ));
                }
                other => {
                    self.push_event("control", render_control_entry_line(&other));
                    self.append_current_session_control(other);
                }
            },
            RunEvent::ContinuationState(state) => {
                self.push_event("continuation", state.state_kind);
            }
            RunEvent::AssistantMessage(message) => {
                for call in &message.tool_calls {
                    self.safe_tool_calls.insert(call.id.clone(), call.clone());
                }
                if let Some(tool_name) = message.tool_calls.first().map(|call| call.name.clone()) {
                    self.runtime.run_phase = RunPhase::Tool(tool_name.clone());
                    self.push_phase_marker(format!("tool|{tool_name}"));
                } else {
                    self.runtime.run_phase = RunPhase::Streaming;
                    self.push_phase_marker("streaming".to_owned());
                }
                if message.assistant_kind == Some(sigil_kernel::AssistantMessageKind::ToolPreamble)
                {
                    self.downgrade_streaming_assistant_entry_to_thinking();
                }
                self.finish_streaming_assistant_entry();
                if message
                    .tool_calls
                    .iter()
                    .any(|call| suppress_reasoning_before_tool_call(call.name.as_str()))
                {
                    self.discard_streaming_reasoning_entry();
                } else {
                    self.finish_streaming_reasoning_entry();
                }
                if message.assistant_kind != Some(sigil_kernel::AssistantMessageKind::ToolPreamble)
                    && let Some(content) = message.content
                {
                    if message.assistant_kind
                        == Some(sigil_kernel::AssistantMessageKind::FinalAnswer)
                    {
                        self.push_final_assistant_message_once(content);
                    } else {
                        self.push_assistant_message_once(content);
                    }
                }
            }
            RunEvent::Notice(note) => {
                let rejects_current_final_candidate = notice_rejects_current_final_candidate(&note);
                if rejects_current_final_candidate {
                    self.discard_streaming_assistant_entry();
                }
                self.last_notice = Some(note.clone());
                if rejects_current_final_candidate || notice_is_timeline_worthy(&note) {
                    self.push_timeline(TimelineRole::Notice, note.clone());
                }
                self.push_event("notice", note);
            }
        }
        Ok(())
    }
}
