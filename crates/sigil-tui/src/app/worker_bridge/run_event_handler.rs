use anyhow::Result;
use sigil_kernel::{
    ControlEntry, EventHandler, RunEvent, ToolDiffBudget, ToolExecutionStatus, ToolPreviewSnapshot,
};

use super::super::{
    AppState, ApprovalAction, PaneFocus, PendingApproval, RunPhase, TimelineRole,
    formatting::{
        format_agent_thread_started_block, format_agent_thread_status_block,
        format_terminal_task_block_redacted, format_tool_result_block_redacted,
    },
    session_flow::render_control_entry_line,
};
use super::{
    run_event_helpers::{
        notice_is_timeline_worthy, notice_rejects_current_final_candidate, spawn_agent_profile_id,
    },
    tool_card_lifecycle::{
        agent_tool_name, suppress_reasoning_before_tool_call, tool_card_replacement_indices,
        tool_progress_result, wait_agent_pending_replacement_indices,
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
                let session_grant_available =
                    sigil_kernel::tool_approval_session_grant_available_for_facets(
                        spec.access,
                        network_effect,
                        operation,
                        risk,
                        &subjects,
                        &subject_zones,
                        confirmation.as_ref(),
                        snapshot_required,
                        local_policy_decision,
                        network_policy_decision,
                        source_policy_decision,
                    );
                self.approval.pending = Some(PendingApproval {
                    call: call.clone(),
                    session_grant_available,
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
                });
                self.active_pane = PaneFocus::Activity;
                self.approval.scroll_back = 0;
                self.approval.metadata_collapsed = false;
                self.approval.selected_file_index = 0;
                self.approval.selected_hunk_index = 0;
                self.approval.selected_action =
                    ApprovalAction::default_for(risk, session_grant_available);
                self.last_notice = Some(format!("approve {}", call.name));
                self.push_event("approval:request", format!("{} {}", call.name, call.id));
                self.push_timeline(
                    TimelineRole::Notice,
                    format!("Approve {}? Y allow once, N deny.", call.name),
                );
            }
            RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            } => {
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
            RunEvent::ToolProgress(progress) => {
                self.runtime.run_phase = RunPhase::Tool(progress.tool_name.clone());
                self.finish_streaming_assistant_entry();
                self.finish_streaming_reasoning_entry();
                self.push_phase_marker(format!("tool|{}", progress.tool_name));
                let result = tool_progress_result(progress);
                let rendered =
                    format_tool_result_block_redacted(&result, None, &self.secret_redactor);
                if let Some(indices) = tool_card_replacement_indices(&self.timeline, &rendered) {
                    self.replace_tool_timeline_entries(&indices, rendered);
                } else {
                    self.push_timeline(TimelineRole::Tool, rendered);
                }
                self.push_event(
                    "tool:progress",
                    format!("{} {}", result.tool_name, result.content),
                );
            }
            RunEvent::ToolResult(result) => {
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
                let preview = self.tool_preview_snapshots.get(&result.call_id);
                let rendered =
                    format_tool_result_block_redacted(&result, preview, &self.secret_redactor);
                if let Some(indices) =
                    wait_agent_pending_replacement_indices(&self.timeline, &result, &rendered)
                {
                    self.replace_tool_timeline_entries(&indices, rendered);
                } else if let Some(indices) =
                    tool_card_replacement_indices(&self.timeline, &rendered)
                {
                    self.replace_tool_timeline_entries(&indices, rendered);
                } else {
                    self.push_timeline(TimelineRole::Tool, rendered);
                }
                self.push_event("tool:result", format!("{} {}", result.tool_name, status));
            }
            RunEvent::Usage(usage) => {
                self.runtime.stats.apply_usage(&usage);
                self.runtime.session_delta_stats.apply_usage(&usage);
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
                    self.replace_or_push_tool_card(format_terminal_task_block_redacted(
                        &task,
                        &self.secret_redactor,
                    ));
                    self.append_current_session_control(ControlEntry::TerminalTask(task));
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
