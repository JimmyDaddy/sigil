use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use sigil_kernel::{
    CompactionPreview, ControlEntry, ExternalEvidenceLevel, ExternalProvenanceEntry, ModelMessage,
    SessionLogEntry, ToolCall, ToolEgressEntry, ToolExecutionEntry, ToolExecutionStatus,
    ToolPreviewSnapshot,
};

use super::super::formatting::truncate_session_view_text;

pub(super) fn render_model_message_line(message: &ModelMessage) -> String {
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

pub(super) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub(super) fn render_session_log_entry(entry: &SessionLogEntry) -> String {
    match entry {
        SessionLogEntry::User(message)
        | SessionLogEntry::Assistant(message)
        | SessionLogEntry::ToolResult(message) => render_model_message_line(message),
        SessionLogEntry::Control(control) => render_control_entry_line(control),
    }
}

pub(in crate::app) fn render_control_entry_line(control: &ControlEntry) -> String {
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
        ControlEntry::ExternalProvenance(entry) => render_external_provenance_line(entry),
        ControlEntry::WebUrlCapabilityDescriptor(entry) => format!(
            "[ctl] url capability source={} message={} restart={:?}",
            truncate_session_view_text(&entry.source_id, 48),
            truncate_session_view_text(&entry.durable_entry_id, 48),
            entry.restart_policy
        ),
        ControlEntry::UsageSnapshot(usage) => format!(
            "[ctl] usage p={} c={} hit={} miss={}",
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cache_hit_tokens,
            usage.cache_miss_tokens
        ),
        ControlEntry::ToolApproval(approval) => format!(
            "[ctl] approval {} {} action={} effect={} local={} network={} source={} final={}",
            approval.call_id,
            approval.tool_name,
            tool_approval_action_label(approval.action),
            approval
                .network_effect
                .map_or("none", sigil_kernel::NetworkEffect::as_str),
            approval.local_policy_decision.as_str(),
            approval.network_policy_decision.as_str(),
            approval.source_policy_decision.as_str(),
            approval.policy_decision.as_str()
        ),
        ControlEntry::ToolApprovalSessionGrant(grant) => format!(
            "[ctl] approval grant {} {} expires=session scope={} facets={} access={} effect={} subjects={}",
            grant.call_id,
            grant.tool_name,
            grant.scope.as_str(),
            grant
                .facets
                .iter()
                .map(|facet| facet.as_str())
                .collect::<Vec<_>>()
                .join("+"),
            grant.access.as_str(),
            grant
                .network_effect
                .map_or("none", sigil_kernel::NetworkEffect::as_str),
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
            "[ctl] plugin hook {}:{} started kind={} effect={} backend={} profile={} coverage={} env={}",
            truncate_session_view_text(&entry.plugin_id, 32),
            truncate_session_view_text(&entry.hook_id, 32),
            format!("{:?}", entry.hook_kind).to_ascii_lowercase(),
            entry.declared_effect.as_str(),
            entry.backend.as_str(),
            entry.sandbox_profile.as_str(),
            entry.execution_coverage.as_str(),
            entry.environment_policy.as_str()
        ),
        ControlEntry::PluginHookExecutionFinished(entry) => format!(
            "[ctl] plugin hook {}:{} finished status={} exit={} stdout={} stderr={} backend={} network={} env={}",
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
            entry.network.policy.as_str(),
            entry.environment_policy.as_str()
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
            "[ctl] plan grant v{} permission={} expires={} hash={}",
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

fn render_external_provenance_line(entry: &ExternalProvenanceEntry) -> String {
    const MAX_VISIBLE_SOURCES: usize = 3;
    const MAX_VISIBLE_CITATIONS: usize = 3;

    let mut lines = vec![format!(
        "[ctl] external provenance message={} sources={} citations={} trust={:?}",
        truncate_session_view_text(&entry.message_id, 48),
        entry.sources.len(),
        entry.citations.len(),
        entry.trust
    )];

    for source in entry.sources.iter().take(MAX_VISIBLE_SOURCES) {
        let title = source.title.as_deref().unwrap_or("untitled");
        lines.push(format!(
            "  source id={} level={} title={} url={}",
            truncate_session_view_text(&source.source_id, 24),
            external_evidence_level_label(source.evidence_level),
            truncate_session_view_text(title, 72),
            truncate_session_view_text(&source.safe_display_url, 96)
        ));
    }
    if entry.sources.len() > MAX_VISIBLE_SOURCES {
        lines.push(format!(
            "  sources: {} additional source(s) hidden",
            entry.sources.len() - MAX_VISIBLE_SOURCES
        ));
    }

    for citation in entry.citations.iter().take(MAX_VISIBLE_CITATIONS) {
        lines.push(format!(
            "  citation source={} range={}..{}",
            truncate_session_view_text(&citation.source_id, 24),
            citation.start_byte,
            citation.end_byte
        ));
    }
    if entry.citations.len() > MAX_VISIBLE_CITATIONS {
        lines.push(format!(
            "  citations: {} additional citation(s) hidden",
            entry.citations.len() - MAX_VISIBLE_CITATIONS
        ));
    }

    lines.join("\n")
}

fn external_evidence_level_label(level: ExternalEvidenceLevel) -> &'static str {
    match level {
        ExternalEvidenceLevel::SearchSnippet => "search_snippet",
        ExternalEvidenceLevel::ProviderGroundingSource => "provider_grounding_source",
        ExternalEvidenceLevel::FetchedPage => "fetched_page",
    }
}

pub(super) fn render_tool_execution_line(execution: &ToolExecutionEntry) -> String {
    format!(
        "[ctl] execution {} {} status={}",
        execution.call_id,
        execution.tool_name,
        tool_execution_status_label(execution.status)
    )
}

pub(super) fn render_tool_egress_line(egress: &ToolEgressEntry) -> String {
    format!(
        "[ctl] egress {} {} dest={} op={} redacted={}",
        egress.call_id,
        egress.tool_name,
        truncate_session_view_text(&egress.destination, 48),
        truncate_session_view_text(&egress.operation, 32),
        egress.redacted
    )
}

pub(super) fn restored_tool_execution_index(
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

pub(super) fn restored_tool_call_index(entries: &[SessionLogEntry]) -> HashMap<String, ToolCall> {
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

pub(super) fn restored_tool_preview_snapshot_index(
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

pub(super) fn restored_tool_result_call_ids(entries: &[SessionLogEntry]) -> HashSet<String> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResult(message) => message.tool_call_id.clone(),
            _ => None,
        })
        .collect()
}

pub(super) fn should_render_restored_tool_execution(
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

pub(super) fn restored_tool_execution_content(execution: &ToolExecutionEntry) -> String {
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

pub(super) fn restored_reasoning_note(kind: &str, data: &serde_json::Value) -> Option<String> {
    let field = if kind == "reasoning_trace" {
        "text"
    } else {
        "delta"
    };
    data.get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

pub(super) fn tool_approval_action_label(
    action: sigil_kernel::ToolApprovalAuditAction,
) -> &'static str {
    match action {
        sigil_kernel::ToolApprovalAuditAction::PolicyEvaluated => "policy",
        sigil_kernel::ToolApprovalAuditAction::Requested => "requested",
        sigil_kernel::ToolApprovalAuditAction::Resolved => "resolved",
        sigil_kernel::ToolApprovalAuditAction::PreviewFailed => "preview_failed",
    }
}

pub(super) fn mcp_elicitation_decision_label(
    decision: sigil_kernel::McpElicitationDecision,
) -> &'static str {
    match decision {
        sigil_kernel::McpElicitationDecision::Accepted => "accepted",
        sigil_kernel::McpElicitationDecision::Declined => "declined",
        sigil_kernel::McpElicitationDecision::Cancelled => "cancelled",
    }
}

pub(super) fn tool_execution_status_label(
    status: sigil_kernel::ToolExecutionStatus,
) -> &'static str {
    match status {
        sigil_kernel::ToolExecutionStatus::Started => "started",
        sigil_kernel::ToolExecutionStatus::Completed => "completed",
        sigil_kernel::ToolExecutionStatus::Failed => "failed",
        sigil_kernel::ToolExecutionStatus::Cancelled => "cancelled",
        sigil_kernel::ToolExecutionStatus::Interrupted => "interrupted",
    }
}

pub(super) fn task_run_status_label(status: sigil_kernel::TaskRunStatus) -> &'static str {
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

pub(super) fn task_plan_status_label(status: sigil_kernel::TaskPlanStatus) -> &'static str {
    match status {
        sigil_kernel::TaskPlanStatus::Proposed => "proposed",
        sigil_kernel::TaskPlanStatus::Accepted => "accepted",
        sigil_kernel::TaskPlanStatus::Superseded => "superseded",
        sigil_kernel::TaskPlanStatus::Rejected => "rejected",
    }
}

pub(super) fn plan_approval_permission_label(
    permission: sigil_kernel::PlanApprovalPermission,
) -> &'static str {
    match permission {
        sigil_kernel::PlanApprovalPermission::Ask => "ask",
        sigil_kernel::PlanApprovalPermission::WorkspaceEdits => "workspace_edits",
    }
}

pub(super) fn plan_approval_expiry_label(
    expiry: &sigil_kernel::PlanApprovalExpiry,
) -> &'static str {
    match expiry {
        sigil_kernel::PlanApprovalExpiry::NextUserPrompt => "next_user_prompt",
        sigil_kernel::PlanApprovalExpiry::Session => "session",
        sigil_kernel::PlanApprovalExpiry::AtUnixMs(_) => "at_unix_ms",
    }
}

pub(super) fn task_step_status_label(status: sigil_kernel::TaskStepStatus) -> &'static str {
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

pub(super) fn task_child_session_status_label(
    status: sigil_kernel::TaskChildSessionStatus,
) -> &'static str {
    match status {
        sigil_kernel::TaskChildSessionStatus::Started => "started",
        sigil_kernel::TaskChildSessionStatus::Completed => "completed",
        sigil_kernel::TaskChildSessionStatus::Failed => "failed",
        sigil_kernel::TaskChildSessionStatus::Cancelled => "cancelled",
        sigil_kernel::TaskChildSessionStatus::Interrupted => "interrupted",
        sigil_kernel::TaskChildSessionStatus::Unavailable => "unavailable",
    }
}

pub(super) fn step_lease_status_label(status: sigil_kernel::StepLeaseStatus) -> &'static str {
    match status {
        sigil_kernel::StepLeaseStatus::Acquired => "acquired",
        sigil_kernel::StepLeaseStatus::Released => "released",
        sigil_kernel::StepLeaseStatus::Interrupted => "interrupted",
        sigil_kernel::StepLeaseStatus::Abandoned => "abandoned",
    }
}

pub(super) fn write_lease_scope_label(scope: &sigil_kernel::WriteLeaseScope) -> &'static str {
    match scope {
        sigil_kernel::WriteLeaseScope::Workspace => "workspace",
        sigil_kernel::WriteLeaseScope::Subjects(_) => "subjects",
    }
}

pub(super) fn run_status_label(status: sigil_kernel::RunStatus) -> &'static str {
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

pub(super) fn verification_verdict_label(
    status: sigil_kernel::VerificationVerdict,
) -> &'static str {
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

pub(super) fn receipt_status_label(status: sigil_kernel::ReceiptStatus) -> &'static str {
    match status {
        sigil_kernel::ReceiptStatus::Succeeded => "succeeded",
        sigil_kernel::ReceiptStatus::Failed => "failed",
        sigil_kernel::ReceiptStatus::Skipped => "skipped",
        sigil_kernel::ReceiptStatus::Inconclusive => "inconclusive",
    }
}

pub(super) fn verification_check_run_status_label(
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

pub(super) fn readiness_required_actions_label(actions: &[sigil_kernel::RequiredAction]) -> String {
    summarized_readiness_items(actions, required_action_label)
}

pub(super) fn readiness_reasons_label(reasons: &[sigil_kernel::ReadinessReason]) -> String {
    summarized_readiness_items(reasons, readiness_reason_label)
}

pub(super) fn summarized_readiness_items<T>(items: &[T], labeler: fn(&T) -> String) -> String {
    let Some(first) = items.first() else {
        return "none".to_owned();
    };
    let mut label = labeler(first);
    if items.len() > 1 {
        label.push_str(&format!("+{}", items.len() - 1));
    }
    truncate_session_view_text(&label, 48)
}

pub(super) fn required_action_label(action: &sigil_kernel::RequiredAction) -> String {
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

pub(super) fn readiness_reason_label(reason: &sigil_kernel::ReadinessReason) -> String {
    match reason {
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

pub(super) fn verification_stale_reason_label(
    reason: &sigil_kernel::VerificationStaleReason,
) -> String {
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

pub(super) fn child_verification_link_status_label(
    entry: &sigil_kernel::ChildVerificationReceiptLinked,
) -> &'static str {
    if entry.merge_event_id.is_some() {
        "merged"
    } else {
        "linked"
    }
}

pub(super) fn child_verification_parent_recheck_label(
    entry: &sigil_kernel::ChildVerificationReceiptLinked,
) -> &'static str {
    if entry.merge_event_id.is_some() {
        "required"
    } else {
        "not_required"
    }
}

pub(super) fn workspace_trust_label(trust: sigil_kernel::WorkspaceTrust) -> &'static str {
    match trust {
        sigil_kernel::WorkspaceTrust::Unknown => "unknown",
        sigil_kernel::WorkspaceTrust::Trusted => "trusted",
        sigil_kernel::WorkspaceTrust::Restricted => "restricted",
        sigil_kernel::WorkspaceTrust::Denied => "denied",
    }
}

pub(super) fn check_discovery_source_label(
    source: sigil_kernel::CheckDiscoverySource,
) -> &'static str {
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

pub(super) fn check_promotion_label(promotion: &sigil_kernel::CheckPromotion) -> &'static str {
    match promotion {
        sigil_kernel::CheckPromotion::UserApproved { .. } => "user_approved",
        sigil_kernel::CheckPromotion::WorkspaceTrusted { .. } => "workspace_trusted",
        sigil_kernel::CheckPromotion::Sandboxed { .. } => "sandboxed",
        sigil_kernel::CheckPromotion::GlobalPolicy { .. } => "global_policy",
        sigil_kernel::CheckPromotion::ExplicitUserConfig { .. } => "explicit_user_config",
    }
}

pub(super) fn evidence_scope_label(scope: &sigil_kernel::EvidenceScope) -> String {
    match scope {
        sigil_kernel::EvidenceScope::Run(id) => format!("run:{id}"),
        sigil_kernel::EvidenceScope::Workspace(id) => format!("workspace:{id}"),
        sigil_kernel::EvidenceScope::Task(id) => format!("task:{id}"),
        sigil_kernel::EvidenceScope::Step(id) => format!("step:{id}"),
        sigil_kernel::EvidenceScope::Agent(id) => format!("agent:{id}"),
        sigil_kernel::EvidenceScope::Changeset(id) => format!("changeset:{id}"),
    }
}

pub(super) fn task_route_status_label(status: sigil_kernel::TaskRouteStatus) -> &'static str {
    match status {
        sigil_kernel::TaskRouteStatus::Registered => "registered",
        sigil_kernel::TaskRouteStatus::Requested => "requested",
        sigil_kernel::TaskRouteStatus::Resolved => "resolved",
        sigil_kernel::TaskRouteStatus::Rejected => "rejected",
        sigil_kernel::TaskRouteStatus::Cancelled => "cancelled",
        sigil_kernel::TaskRouteStatus::Stale => "stale",
    }
}

pub(super) fn agent_trust_state_label(status: sigil_kernel::AgentTrustState) -> &'static str {
    match status {
        sigil_kernel::AgentTrustState::Trusted => "trusted",
        sigil_kernel::AgentTrustState::NeedsReview => "needs_review",
        sigil_kernel::AgentTrustState::Disabled => "disabled",
        sigil_kernel::AgentTrustState::Unknown => "unknown",
    }
}

pub(super) fn optional_bool_label(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "inherit",
    }
}

pub(super) fn agent_invocation_mode_label(mode: sigil_kernel::AgentInvocationMode) -> &'static str {
    match mode {
        sigil_kernel::AgentInvocationMode::Foreground => "foreground",
        sigil_kernel::AgentInvocationMode::Background => "background",
        sigil_kernel::AgentInvocationMode::JoinBeforeFinal => "join_before_final",
        sigil_kernel::AgentInvocationMode::Unknown => "unknown",
    }
}

pub(super) fn agent_thread_status_label(status: sigil_kernel::AgentThreadStatus) -> &'static str {
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

pub(super) fn agent_terminal_status_label(
    status: sigil_kernel::AgentThreadTerminalStatus,
) -> &'static str {
    match status {
        sigil_kernel::AgentThreadTerminalStatus::Completed => "completed",
        sigil_kernel::AgentThreadTerminalStatus::Failed => "failed",
        sigil_kernel::AgentThreadTerminalStatus::Cancelled => "cancelled",
        sigil_kernel::AgentThreadTerminalStatus::Interrupted => "interrupted",
        sigil_kernel::AgentThreadTerminalStatus::Unknown => "unknown",
    }
}

pub(super) fn agent_route_status_label(status: sigil_kernel::AgentRouteStatus) -> &'static str {
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

pub(super) fn agent_mailbox_status_label(status: sigil_kernel::AgentMailboxStatus) -> &'static str {
    match status {
        sigil_kernel::AgentMailboxStatus::Queued => "queued",
        sigil_kernel::AgentMailboxStatus::Delivered => "delivered",
        sigil_kernel::AgentMailboxStatus::Consumed => "consumed",
        sigil_kernel::AgentMailboxStatus::Rejected => "rejected",
        sigil_kernel::AgentMailboxStatus::Interrupted => "interrupted",
        sigil_kernel::AgentMailboxStatus::Unknown => "unknown",
    }
}

pub(super) fn plugin_hook_execution_status_label(
    status: sigil_kernel::PluginHookExecutionStatus,
) -> &'static str {
    match status {
        sigil_kernel::PluginHookExecutionStatus::Succeeded => "succeeded",
        sigil_kernel::PluginHookExecutionStatus::Failed => "failed",
        sigil_kernel::PluginHookExecutionStatus::TimedOut => "timed_out",
    }
}

pub(super) fn render_compaction_preview_lines(preview: &CompactionPreview) -> Vec<String> {
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
