use anyhow::Result;

use crate::{
    AgentInvocationMode, AgentInvocationSource, AgentProfileCapturedEntry, AgentProfileId,
    AgentProfileSnapshot, AgentProfileSnapshotId, AgentProfileSource, AgentRouteId,
    AgentRouteStatus, AgentThreadId, AgentThreadMessageRoutedEntry, AgentThreadResult,
    AgentThreadResultRecordedEntry, AgentThreadStartedEntry, AgentThreadStateProjection,
    AgentThreadTerminalStatus, AgentTrustState, AgentUsageSummary, ApprovalMode, CandidateCheck,
    CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec, CheckSpecRecordedEntry,
    ChildVerificationReceiptLinked, CompletionCriteria, ControlEntry, DispatchTraceKind,
    DispatchTraceProjectionSnapshot, DispatchTraceStatus, DurableEventType, EventClass,
    EvidenceReceipt, EvidenceScope, FileProjectionStore, JsonlSessionStore, PathTrustZone,
    PermissionRisk, ProjectionApplyDecision, ProjectionPressureReason, ProjectionPressureSample,
    ProjectionPressureThresholds, ProjectionQueryContract, ProjectionQueryFamily,
    ProjectionQueryScope, ProjectionQuerySurface, ProjectionStore, ProjectionStoreRecommendation,
    ReadinessEvaluatedEntry, ReadinessEvaluation, ReceiptStatus, RedactionState, RequiredAction,
    RunStatus, SandboxProfileRequirement, SessionListProjectionSnapshot, SessionLogEntry,
    SessionRef, SessionStreamRecord, TaskId, TaskRunEntry, TaskRunStatus, ToolAccess,
    ToolApprovalAuditAction, ToolApprovalEntry, ToolApprovalUserDecision, ToolEffect, ToolError,
    ToolErrorKind, ToolExecutionEntry, ToolExecutionStatus, ToolOperation, ToolResultMeta,
    ToolSubjectAudit, ToolSubjectKind, ToolSubjectScope, UsageStats, VerificationAutoRunPolicy,
    VerificationBinding, VerificationCheckRunEntry, VerificationCheckRunStatus, VerificationPolicy,
    VerificationPolicyChangedEntry, VerificationRecordedEntry, VerificationScope,
    VerificationStateProjection, VerificationStateProjectionSnapshot, VerificationVerdict,
    VisibleCompletionState, WorkspaceRootSnapshot, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    WorkspaceTrustRequirement, agent_graph_projection_from_records,
    dispatch_trace_projection_from_records, evaluate_projection_pressure,
    session_list_projection_from_records,
};

fn workspace_trust_entry(workspace_id: &str, trust_event: &str) -> WorkspaceTrustDecisionEntry {
    WorkspaceTrustDecisionEntry {
        workspace_id: workspace_id.to_owned(),
        workspace_trust_snapshot_id: format!("{workspace_id}-trust"),
        trust: WorkspaceTrust::Trusted,
        decided_by_event_id: Some(trust_event.to_owned()),
        reason: Some("user trusted workspace".to_owned()),
    }
}

fn append_trust_event(
    store: &JsonlSessionStore,
    workspace_id: &str,
    trust_event: &str,
) -> Result<crate::StoredEvent> {
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::WorkspaceTrustDecision(workspace_trust_entry(workspace_id, trust_event)),
    ))
}

fn read_records(store: &JsonlSessionStore) -> Result<Vec<SessionStreamRecord>> {
    JsonlSessionStore::read_event_records(store.path())
}

fn sample_check_spec() -> CheckSpec {
    CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "-p".to_owned(),
                "sigil-kernel".to_owned(),
            ],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        "scope-main",
    )
}

fn sample_verification_policy() -> VerificationPolicy {
    VerificationPolicy {
        required_checks: vec![sample_check_spec()],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked("scope-main"),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: Some(60_000),
        auto_run: VerificationAutoRunPolicy::Manual,
    }
}

fn sample_check_spec_entry() -> CheckSpecRecordedEntry {
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::Cargo,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "-p".to_owned(),
                "sigil-kernel".to_owned(),
            ],
            cwd: None,
        },
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        "cargo-test",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::UserApproved {
            approval_event_id: "event-approval".to_owned(),
        },
    )
    .expect("sample check promotes");
    CheckSpecRecordedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        trusted_check,
        "event-discovery",
    )
}

fn sample_policy_entry() -> Result<VerificationPolicyChangedEntry> {
    VerificationPolicyChangedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        sample_verification_policy(),
        "event-policy",
    )
}

fn sample_check_run_entry() -> VerificationCheckRunEntry {
    let check = sample_check_spec();
    VerificationCheckRunEntry {
        run_id: "check-run-1".to_owned(),
        scope: EvidenceScope::Task("task-1".to_owned()),
        check_spec_id: check.check_spec_id,
        check_spec_hash: check.check_spec_hash,
        status: VerificationCheckRunStatus::Succeeded,
        receipt_id: Some("receipt-1".to_owned()),
        source_event_id: Some("event-check-finished".to_owned()),
        timeout_ms: Some(60_000),
        reason: None,
    }
}

fn sample_receipt_entry() -> VerificationRecordedEntry {
    let check = sample_check_spec();
    VerificationRecordedEntry {
        receipt: crate::VerificationReceipt {
            receipt: EvidenceReceipt {
                receipt_id: "receipt-1".to_owned(),
                source_session_id: "session-1".to_owned(),
                source_event_id: "event-check-finished".to_owned(),
                source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
                scope: EvidenceScope::Task("task-1".to_owned()),
                producer_tool_call: Some("tool-call-1".to_owned()),
                workspace_revision: Some(1),
                workspace_snapshot_id: Some("snapshot-1".to_owned()),
                policy_hash: Some("policy-hash".to_owned()),
                changeset_id: None,
                status: ReceiptStatus::Succeeded,
                artifact_refs: Vec::new(),
                redaction_state: RedactionState::None,
                recorded_at_stream_sequence: 2,
            },
            binding: VerificationBinding {
                workspace_id: "workspace-1".to_owned(),
                workspace_snapshot_id: "snapshot-1".to_owned(),
                verification_scope_hash: "scope-main".to_owned(),
                check_spec_hash: check.check_spec_hash,
                environment_fingerprint: "env-1".to_owned(),
                sandbox_profile_hash: "sandbox-local".to_owned(),
                execution_backend: None,
                execution_backend_capabilities: None,
                execution_network: Default::default(),
                workspace_trust_snapshot_id: "trust-1".to_owned(),
                approval_event_id: None,
                sandbox_decision_id: None,
            },
            check_spec_id: check.check_spec_id,
            check_status: ReceiptStatus::Succeeded,
            failure_reason: None,
            mutates_verification_scope: false,
        },
    }
}

fn sample_readiness_entry() -> ReadinessEvaluatedEntry {
    ReadinessEvaluatedEntry {
        scope: EvidenceScope::Task("task-1".to_owned()),
        evaluation: ReadinessEvaluation {
            run_status: RunStatus::Completed,
            verification_verdict: VerificationVerdict::Missing,
            visible_state: VisibleCompletionState::CompletedUnverified,
            reasons: Vec::new(),
            required_actions: vec![RequiredAction::RunCheck {
                check_spec_id: "cargo-test".to_owned(),
            }],
        },
        policy_hash: Some("policy-hash".to_owned()),
        workspace_snapshot_id: Some("snapshot-1".to_owned()),
    }
}

fn sample_child_link() -> ChildVerificationReceiptLinked {
    ChildVerificationReceiptLinked {
        parent_session_id: "parent-session".to_owned(),
        child_session_id: "child-session".to_owned(),
        child_receipt_id: "child-receipt".to_owned(),
        child_event_id: "child-event".to_owned(),
        child_workspace_id: "child-workspace".to_owned(),
        child_workspace_snapshot_id: "child-snapshot".to_owned(),
        policy_hash: "policy-hash".to_owned(),
        changeset_id: Some("changeset-1".to_owned()),
        merge_event_id: Some("merge-event".to_owned()),
    }
}

fn sample_usage() -> UsageStats {
    UsageStats {
        prompt_tokens: 11,
        completion_tokens: 7,
        cache_hit_tokens: 3,
        cache_miss_tokens: 8,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }
}

fn sample_task_run_entry() -> Result<TaskRunEntry> {
    Ok(TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("session-parent.jsonl")?,
        objective: "fix selector projection".to_owned(),
        status: TaskRunStatus::Paused,
        reason: Some("needs verification".to_owned()),
    })
}

fn sample_agent_profile_snapshot() -> Result<AgentProfileSnapshot> {
    Ok(AgentProfileSnapshot {
        snapshot_id: AgentProfileSnapshotId::new("snapshot_explore_1")?,
        profile_id: AgentProfileId::new("explore")?,
        source: AgentProfileSource::System,
        source_hash: "sha256:source".to_owned(),
        profile_hash: "sha256:profile".to_owned(),
        resolved_tool_scope_hash: "sha256:tools".to_owned(),
        resolved_permission_policy_hash: "sha256:permissions".to_owned(),
        resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
        resolved_skill_hashes: Vec::new(),
        trust_state: AgentTrustState::Trusted,
    })
}

fn sample_agent_started_entry() -> Result<AgentThreadStartedEntry> {
    Ok(AgentThreadStartedEntry {
        thread_id: AgentThreadId::new("thread_1")?,
        parent_thread_id: Some(AgentThreadId::new("main")?),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        thread_session_ref: SessionRef::new_relative("children/thread_1.jsonl")?,
        profile_id: AgentProfileId::new("explore")?,
        profile_snapshot_id: AgentProfileSnapshotId::new("snapshot_explore_1")?,
        run_context: crate::AgentRunContextSnapshot {
            profile_snapshot_id: AgentProfileSnapshotId::new("snapshot_explore_1")?,
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            reasoning_effort: None,
            workspace_root: WorkspaceRootSnapshot::new("/workspace")?,
            effective_tool_scope_hash: "sha256:tools".to_owned(),
            effective_permission_policy_hash: "sha256:permissions".to_owned(),
            effective_mcp_scope_hash: "sha256:mcp".to_owned(),
            provider_capability_hash: "sha256:provider".to_owned(),
            model_visible_agent_index_hash: Some("sha256:index".to_owned()),
            budget_policy_hash: "sha256:budget".to_owned(),
            provider_background_handle_ref: None,
        },
        objective: "inspect kernel".to_owned(),
        prompt_hash: "sha256:prompt".to_owned(),
        invocation_mode: AgentInvocationMode::Background,
        invocation_source: AgentInvocationSource::Task,
        display_name: Some("kernel map".to_owned()),
        created_at_ms: Some(42),
    })
}

fn sample_agent_result_entry() -> Result<AgentThreadResultRecordedEntry> {
    Ok(AgentThreadResultRecordedEntry {
        result: AgentThreadResult {
            thread_id: AgentThreadId::new("thread_1")?,
            session_ref: SessionRef::new_relative("children/thread_1.jsonl")?,
            status: AgentThreadTerminalStatus::Completed,
            summary: "done".to_owned(),
            summary_truncated: false,
            original_summary_chars: None,
            artifacts: Vec::new(),
            changed_paths: vec!["src/lib.rs".to_owned()],
            risks: Vec::new(),
            followups: Vec::new(),
            usage: Some(AgentUsageSummary {
                input_tokens: 7,
                output_tokens: 5,
                total_tokens: 12,
                cached_tokens: Some(3),
            }),
            output_hash: "sha256:done".to_owned(),
            final_answer_ref: None,
        },
    })
}

fn sample_tool_subject(scope: ToolSubjectScope) -> ToolSubjectAudit {
    ToolSubjectAudit {
        kind: ToolSubjectKind::Path,
        original: "src/lib.rs".to_owned(),
        normalized: "src/lib.rs".to_owned(),
        canonical_path: Some("/workspace/src/lib.rs".to_owned()),
        scope,
    }
}

fn sample_tool_approval_entry(action: ToolApprovalAuditAction) -> ToolApprovalEntry {
    ToolApprovalEntry {
        action,
        call_id: "call_1".to_owned(),
        tool_name: "write_file".to_owned(),
        access: ToolAccess::Write,
        operation: Some(ToolOperation::EditFile),
        risk: Some(PermissionRisk::Medium),
        subjects: vec![sample_tool_subject(ToolSubjectScope::External)],
        subject_zones: vec![PathTrustZone::External],
        policy_decision: ApprovalMode::Ask,
        external_directory_required: true,
        confirmation: None,
        snapshot_required: true,
        user_decision: Some(ToolApprovalUserDecision::Approved),
        reason: Some("approved in test".to_owned()),
        preview_hash: Some("sha256:preview".to_owned()),
    }
}

fn sample_tool_execution_entry(status: ToolExecutionStatus) -> ToolExecutionEntry {
    let mut metadata = ToolResultMeta {
        duration_ms: Some(33),
        bytes: Some(12),
        returned_bytes: Some(12),
        total_bytes: Some(48),
        truncated: true,
        omitted_bytes: Some(36),
        changed_files: vec!["src/lib.rs".to_owned()],
        ..ToolResultMeta::default()
    };
    metadata.exit_code = Some(if status == ToolExecutionStatus::Completed {
        0
    } else {
        1
    });
    ToolExecutionEntry {
        call_id: "call_1".to_owned(),
        tool_name: "write_file".to_owned(),
        status,
        duration_ms: Some(34),
        subjects: vec![sample_tool_subject(ToolSubjectScope::External)],
        changed_files: vec!["src/lib.rs".to_owned()],
        metadata,
        error: (status == ToolExecutionStatus::Failed).then(|| ToolError {
            kind: ToolErrorKind::Internal,
            message: "redacted failure".to_owned(),
            retryable: false,
            details: serde_json::Value::Null,
        }),
        model_content_hash: Some("sha256:model-content".to_owned()),
    }
}

#[test]
fn verification_projection_snapshot_roundtrips_all_entry_vectors() -> Result<()> {
    let snapshot = VerificationStateProjectionSnapshot {
        check_specs: vec![sample_check_spec_entry()],
        policies: vec![sample_policy_entry()?],
        check_runs: vec![sample_check_run_entry()],
        receipts: vec![sample_receipt_entry()],
        readiness: vec![sample_readiness_entry()],
        child_receipt_links: vec![sample_child_link()],
        workspace_trust: vec![workspace_trust_entry("workspace-1", "trust-event")],
    };

    let projection = VerificationStateProjection::from(snapshot.clone());
    let restored = VerificationStateProjectionSnapshot::from(&projection);

    assert_eq!(restored, snapshot);
    Ok(())
}

#[test]
fn session_list_projection_rebuilds_mixed_stream_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
    });
    std::fs::write(
        &session_path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::User(crate::ModelMessage::user(
        "Investigate session projection",
    )))?;
    store.append(&SessionLogEntry::Assistant(crate::ModelMessage::assistant(
        Some("Projection implemented".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        sample_usage(),
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskRun(
        sample_task_run_entry()?,
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
        sample_readiness_entry(),
    )))?;
    let records = read_records(&store)?;

    let projection = session_list_projection_from_records(&records)?;
    let entry = projection
        .latest_session()
        .expect("session projection should contain one entry");

    assert_eq!(entry.provider_name.as_deref(), Some("deepseek"));
    assert_eq!(entry.model_name.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(
        entry.title.as_deref(),
        Some("Investigate session projection")
    );
    assert_eq!(entry.user_message_count, 1);
    assert_eq!(entry.assistant_message_count, 1);
    assert_eq!(entry.control_entry_count, 4);
    assert_eq!(
        entry.latest_usage.as_ref().map(|usage| usage.prompt_tokens),
        Some(11)
    );
    assert_eq!(
        entry.latest_task.as_ref().map(|task| task.status),
        Some(TaskRunStatus::Paused)
    );
    assert_eq!(
        entry
            .latest_readiness
            .as_ref()
            .map(|readiness| readiness.verification_verdict),
        Some(VerificationVerdict::Missing)
    );
    assert_eq!(entry.last_stream_sequence, records.len() as u64);
    Ok(())
}

#[test]
fn file_projection_store_rebuilds_session_list_projection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(crate::ModelMessage::user(
        "List me from projection",
    )))?;
    let records = read_records(&store)?;
    let projection_store = FileProjectionStore::<SessionListProjectionSnapshot>::session_list(
        temp.path().join("session-list.projection.json"),
    );

    let state = projection_store.rebuild_session_list_from_records(&records)?;
    let loaded = projection_store.load()?;

    assert_eq!(state, loaded);
    assert_eq!(
        loaded
            .projection
            .latest_session()
            .and_then(|entry| entry.title.as_deref()),
        Some("List me from projection")
    );
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(1)
    );
    Ok(())
}

#[test]
fn agent_graph_projection_rebuilds_mixed_stream_and_store() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let legacy_profile = SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
        AgentProfileCapturedEntry {
            snapshot: sample_agent_profile_snapshot()?,
        },
    ));
    std::fs::write(
        &session_path,
        format!("{}\n", serde_json::to_string(&legacy_profile)?),
    )?;
    let session_store = JsonlSessionStore::new(&session_path)?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentThreadStarted(sample_agent_started_entry()?),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentThreadMessageRouted(AgentThreadMessageRoutedEntry {
            route_id: AgentRouteId::new("route_1")?,
            source_thread_id: AgentThreadId::new("main")?,
            target_thread_id: AgentThreadId::new("thread_1")?,
            prompt_hash: "sha256:prompt-route".to_owned(),
            prompt: Some("continue inspection".to_owned()),
            status: AgentRouteStatus::Requested,
        }),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentThreadResultRecorded(sample_agent_result_entry()?),
    ))?;
    let records = read_records(&session_store)?;

    let projection = agent_graph_projection_from_records(&records)?;
    let summary = projection.graph_summary();
    let projection_store = FileProjectionStore::<AgentThreadStateProjection>::agent_graph(
        temp.path().join("agent-graph.projection.json"),
    );
    let stored_state = projection_store.rebuild_agent_graph_from_records(&records)?;
    let loaded_state = projection_store.load()?;

    assert_eq!(projection.threads.len(), 1);
    assert_eq!(summary.total_threads, 1);
    assert_eq!(summary.terminal_threads, 1);
    assert_eq!(summary.message_routes, 1);
    assert_eq!(summary.open_routes, 1);
    assert_eq!(summary.total_tokens, 12);
    assert_eq!(summary.cached_tokens, 3);
    assert_eq!(summary.changed_path_count, 1);
    assert_eq!(stored_state, loaded_state);
    assert_eq!(
        loaded_state
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(records.len() as u64)
    );
    assert_eq!(
        loaded_state
            .projection
            .threads
            .get(&AgentThreadId::new("thread_1")?)
            .and_then(|thread| thread.result.as_ref())
            .map(|result| result.summary.as_str()),
        Some("done")
    );
    Ok(())
}

#[test]
fn file_projection_store_applies_agent_graph_idempotently() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentProfileCaptured(AgentProfileCapturedEntry {
            snapshot: sample_agent_profile_snapshot()?,
        }),
    ))?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<AgentThreadStateProjection>::agent_graph(
        temp.path().join("agent-graph.projection.json"),
    );

    let first = projection_store.apply_agent_graph_record(&records[0])?;
    let second = projection_store.apply_agent_graph_record(&records[0])?;
    let loaded = projection_store.load()?;

    assert_eq!(first, ProjectionApplyDecision::Apply);
    assert_eq!(second, ProjectionApplyDecision::IgnoreAlreadyApplied);
    assert_eq!(loaded.projection.profiles.len(), 1);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_event_id.as_str()),
        Some(records[0].event_id())
    );
    Ok(())
}

#[test]
fn dispatch_trace_projection_rebuilds_tool_agent_usage_and_readiness() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ToolApproval(sample_tool_approval_entry(
            ToolApprovalAuditAction::Resolved,
        )),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ToolExecution(Box::new(sample_tool_execution_entry(
            ToolExecutionStatus::Started,
        ))),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ToolEgress(Box::new(crate::ToolEgressEntry {
            call_id: "call_1".to_owned(),
            tool_name: "write_file".to_owned(),
            destination: "filesystem".to_owned(),
            operation: "write".to_owned(),
            subjects: vec![sample_tool_subject(ToolSubjectScope::External)],
            payload: serde_json::json!({"redacted": true, "bytes": 12}),
            redacted: true,
        })),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ToolExecution(Box::new(sample_tool_execution_entry(
            ToolExecutionStatus::Completed,
        ))),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentThreadStarted(sample_agent_started_entry()?),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentThreadResultRecorded(sample_agent_result_entry()?),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::UsageSnapshot(sample_usage()),
    ))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ReadinessEvaluated(sample_readiness_entry()),
    ))?;
    let records = read_records(&session_store)?;

    let projection = dispatch_trace_projection_from_records(&records)?;
    let projection_store = FileProjectionStore::<DispatchTraceProjectionSnapshot>::dispatch_trace(
        temp.path().join("dispatch-trace.projection.json"),
    );
    let stored_state = projection_store.rebuild_dispatch_trace_from_records(&records)?;
    let loaded_state = projection_store.load()?;
    let tool_trace = projection
        .trace("tool:call_1")
        .expect("tool dispatch trace should exist");
    let agent_trace = projection
        .trace("agent:thread_1")
        .expect("agent dispatch trace should exist");

    assert_eq!(tool_trace.kind, DispatchTraceKind::Tool);
    assert_eq!(tool_trace.status, DispatchTraceStatus::Completed);
    assert_eq!(tool_trace.tool_name.as_deref(), Some("write_file"));
    assert_eq!(tool_trace.egress_count, 1);
    assert_eq!(tool_trace.egress_redacted_count, 1);
    assert_eq!(
        tool_trace.egress_destinations,
        vec!["filesystem".to_owned()]
    );
    assert!(tool_trace.observation_truncated);
    assert_eq!(
        tool_trace.model_content_hash.as_deref(),
        Some("sha256:model-content")
    );
    assert_eq!(tool_trace.external_subject_count, 1);
    assert_eq!(agent_trace.kind, DispatchTraceKind::Agent);
    assert_eq!(agent_trace.status, DispatchTraceStatus::Completed);
    assert_eq!(agent_trace.total_tokens, Some(12));
    assert_eq!(projection.summary.total_traces, 2);
    assert_eq!(projection.summary.tool_traces, 1);
    assert_eq!(projection.summary.agent_traces, 1);
    assert_eq!(projection.summary.egress_events, 1);
    assert_eq!(projection.summary.redacted_egress_events, 1);
    assert_eq!(projection.summary.truncated_observations, 1);
    assert_eq!(
        projection
            .latest_usage
            .as_ref()
            .map(|usage| usage.prompt_tokens),
        Some(11)
    );
    assert_eq!(
        projection
            .latest_readiness
            .as_ref()
            .map(|readiness| readiness.visible_state),
        Some(VisibleCompletionState::CompletedUnverified)
    );
    assert_eq!(stored_state, loaded_state);
    assert_eq!(
        loaded_state
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(records.len() as u64)
    );
    Ok(())
}

#[test]
fn dispatch_trace_projection_redacts_payload_and_replays_idempotently() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    session_store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ToolEgress(Box::new(crate::ToolEgressEntry {
            call_id: "call_1".to_owned(),
            tool_name: "webfetch".to_owned(),
            destination: "https://example.test".to_owned(),
            operation: "request".to_owned(),
            subjects: vec![sample_tool_subject(ToolSubjectScope::External)],
            payload: serde_json::json!({"secret": "must-not-project"}),
            redacted: true,
        })),
    ))?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<DispatchTraceProjectionSnapshot>::dispatch_trace(
        temp.path().join("dispatch-trace.projection.json"),
    );

    let first = projection_store.apply_dispatch_trace_record(&records[0])?;
    let second = projection_store.apply_dispatch_trace_record(&records[0])?;
    let loaded = projection_store.load()?;
    let encoded = serde_json::to_string(&loaded.projection)?;

    assert_eq!(first, ProjectionApplyDecision::Apply);
    assert_eq!(second, ProjectionApplyDecision::IgnoreAlreadyApplied);
    assert!(!encoded.contains("must-not-project"));
    let destinations = loaded
        .projection
        .trace("tool:call_1")
        .map(|trace| trace.egress_destinations.clone())
        .unwrap_or_default();
    assert_eq!(destinations, vec!["https://example.test".to_owned()]);
    Ok(())
}

#[test]
fn file_projection_store_rebuilds_verification_projection_from_jsonl() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(
        workspace_trust_entry("workspace-legacy", "legacy-trust-event"),
    ));
    std::fs::write(
        &session_path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let session_store = JsonlSessionStore::new(&session_path)?;
    append_trust_event(&session_store, "workspace-v2", "v2-trust-event")?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );

    let state = projection_store.rebuild_verification_from_records(&records)?;
    let loaded = projection_store.load()?;
    let persisted_projection = VerificationStateProjection::from(loaded.projection.clone());
    let expected_projection = {
        let mut projection = VerificationStateProjection::default();
        projection.apply_control_entry(&ControlEntry::WorkspaceTrustDecision(
            workspace_trust_entry("workspace-legacy", "legacy-trust-event"),
        ));
        projection.apply_control_entry(&ControlEntry::WorkspaceTrustDecision(
            workspace_trust_entry("workspace-v2", "v2-trust-event"),
        ));
        projection
    };

    assert_eq!(state, loaded);
    assert_eq!(persisted_projection, expected_projection);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(2)
    );
    Ok(())
}

#[test]
fn file_projection_store_exposes_trait_and_rebuild_diagnostics() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event")?;
    let records = read_records(&session_store)?;
    let repeated = vec![records[0].clone(), records[0].clone()];
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );

    let output = projection_store.rebuild_stream_records(
        &repeated,
        super::apply_verification_projection_snapshot_record,
    )?;
    let loaded = projection_store.load_state()?;

    assert_eq!(output.state, loaded);
    assert_eq!(output.report.applied_records, 1);
    assert_eq!(output.report.ignored_records, 1);
    assert_eq!(
        output
            .report
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(1)
    );
    Ok(())
}

#[test]
fn file_projection_store_fails_closed_on_invalid_envelopes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("verification.projection.json");
    let projection_store =
        FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(&path);
    let snapshot = serde_json::to_value(VerificationStateProjectionSnapshot::default())?;

    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 999,
            "projection_name": "verification_state",
            "projection_schema_version": crate::session::VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION,
            "state": { "projection": snapshot.clone(), "cursor": null }
        }))?,
    )?;
    let error = projection_store
        .load()
        .expect_err("unsupported store schema should fail closed");
    assert!(
        error
            .to_string()
            .contains("unsupported projection store schema")
    );

    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": crate::FILE_PROJECTION_STORE_SCHEMA_VERSION,
            "projection_name": "other_projection",
            "projection_schema_version": crate::session::VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION,
            "state": { "projection": snapshot.clone(), "cursor": null }
        }))?,
    )?;
    let error = projection_store
        .load()
        .expect_err("wrong projection name should fail closed");
    assert!(error.to_string().contains("projection store contains"));

    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": crate::FILE_PROJECTION_STORE_SCHEMA_VERSION,
            "projection_name": "verification_state",
            "projection_schema_version": 999,
            "state": { "projection": snapshot, "cursor": null }
        }))?,
    )?;
    let error = projection_store
        .load()
        .expect_err("wrong projection schema should fail closed");
    assert!(error.to_string().contains("projection schema 999"));

    std::fs::write(&path, b"{not json")?;
    let error = projection_store
        .load()
        .expect_err("corrupt projection store should fail closed");
    assert!(
        error
            .to_string()
            .contains("failed to parse projection store")
    );
    Ok(())
}

#[test]
fn file_projection_store_ignores_duplicate_current_record_without_rewrite() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event")?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );

    let first = projection_store.apply_verification_record(&records[0])?;
    let second = projection_store.apply_verification_record(&records[0])?;
    let loaded = projection_store.load()?;

    assert_eq!(first, ProjectionApplyDecision::Apply);
    assert_eq!(second, ProjectionApplyDecision::IgnoreAlreadyApplied);
    assert_eq!(loaded.projection.workspace_trust.len(), 1);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_event_id.as_str()),
        Some(records[0].event_id())
    );
    Ok(())
}

#[test]
fn file_projection_store_fails_closed_on_sequence_gap_without_advancing_cursor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event-1")?;
    let mut records = read_records(&session_store)?;
    let gap = crate::StoredEvent::new(
        DurableEventType::WorkspaceTrustDecision,
        EventClass::Critical,
        "event-gap".to_owned(),
        records[0].session_id().to_owned(),
        3,
        serde_json::json!({
            "session_log_entry": SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(
                workspace_trust_entry("workspace-2", "trust-event-2")
            ))
        }),
    )?;
    records.push(SessionStreamRecord::Stored(gap));
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );
    projection_store.apply_verification_record(&records[0])?;

    let error = projection_store
        .apply_verification_record(&records[1])
        .expect_err("sequence gap should fail closed");
    let loaded = projection_store.load()?;

    assert!(error.to_string().contains("projection sequence gap"));
    assert_eq!(loaded.projection.workspace_trust.len(), 1);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(1)
    );
    Ok(())
}

#[test]
fn file_projection_store_fails_closed_when_cursor_is_ahead_of_old_record() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event-1")?;
    append_trust_event(&session_store, "workspace-2", "trust-event-2")?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );
    projection_store.rebuild_verification_from_records(&records)?;

    let error = projection_store
        .apply_verification_record(&records[0])
        .expect_err("cursor ahead of old record should fail closed");
    let loaded = projection_store.load()?;

    assert!(error.to_string().contains("cursor is ahead"));
    assert_eq!(loaded.projection.workspace_trust.len(), 2);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(2)
    );
    Ok(())
}

#[test]
fn projection_pressure_keeps_file_backed_without_product_pressure() {
    let contract = ProjectionQueryContract::new(
        ProjectionQueryFamily::SessionList,
        ProjectionQueryScope::SingleSession,
        ProjectionQuerySurface::Tui,
    );
    let sample = ProjectionPressureSample::new(contract);

    let evaluation =
        evaluate_projection_pressure(&sample, &ProjectionPressureThresholds::default());

    assert_eq!(
        evaluation.recommendation,
        ProjectionStoreRecommendation::KeepFileBacked
    );
    assert_eq!(
        evaluation.reasons,
        vec![ProjectionPressureReason::NoPressure]
    );
}

#[test]
fn projection_pressure_measures_cross_session_product_queries_before_escalating() {
    let contract = ProjectionQueryContract::new(
        ProjectionQueryFamily::SessionList,
        ProjectionQueryScope::CrossSession,
        ProjectionQuerySurface::Desktop,
    )
    .with_pagination(true)
    .with_filtering(true)
    .with_sorting(true);
    let sample = ProjectionPressureSample {
        product_surface_count: 2,
        sessions_scanned: 75,
        records_scanned: 12_000,
        ..ProjectionPressureSample::new(contract)
    };

    let evaluation =
        evaluate_projection_pressure(&sample, &ProjectionPressureThresholds::default());

    assert_eq!(
        evaluation.recommendation,
        ProjectionStoreRecommendation::MeasureMore
    );
    assert!(
        evaluation
            .reasons
            .contains(&ProjectionPressureReason::CrossSessionQuery)
    );
    assert!(
        evaluation
            .reasons
            .contains(&ProjectionPressureReason::PaginationRequired)
    );
    assert!(
        evaluation
            .reasons
            .contains(&ProjectionPressureReason::SessionScanHigh)
    );
}

#[test]
fn projection_pressure_escalates_when_scan_and_latency_cross_thresholds() {
    let contract = ProjectionQueryContract::new(
        ProjectionQueryFamily::DispatchTrace,
        ProjectionQueryScope::CrossSession,
        ProjectionQuerySurface::Http,
    )
    .with_search(true);
    let sample = ProjectionPressureSample {
        sessions_scanned: 300,
        records_scanned: 75_000,
        repeated_log_scans: 6,
        product_surface_count: 3,
        query_latency_ms: Some(450),
        rebuild_latency_ms: Some(2_500),
        ..ProjectionPressureSample::new(contract)
    };

    let evaluation =
        evaluate_projection_pressure(&sample, &ProjectionPressureThresholds::default());

    assert_eq!(
        evaluation.recommendation,
        ProjectionStoreRecommendation::EscalateMaterializedView
    );
    assert!(
        evaluation
            .reasons
            .contains(&ProjectionPressureReason::RecordScanHigh)
    );
    assert!(
        evaluation
            .reasons
            .contains(&ProjectionPressureReason::QueryLatencyHigh)
    );
    assert!(
        evaluation
            .reasons
            .contains(&ProjectionPressureReason::RepeatedLogScanHigh)
    );
}

#[test]
fn projection_pressure_keeps_fresh_runtime_state_out_of_projection_escalation() {
    let contract = ProjectionQueryContract::new(
        ProjectionQueryFamily::AgentGraph,
        ProjectionQueryScope::CrossSession,
        ProjectionQuerySurface::Desktop,
    )
    .with_fresh_live_state(true);
    let sample = ProjectionPressureSample {
        records_scanned: 100_000,
        query_latency_ms: Some(1_000),
        ..ProjectionPressureSample::new(contract)
    };

    let evaluation =
        evaluate_projection_pressure(&sample, &ProjectionPressureThresholds::default());

    assert_eq!(
        evaluation.recommendation,
        ProjectionStoreRecommendation::KeepFileBacked
    );
    assert_eq!(
        evaluation.reasons,
        vec![ProjectionPressureReason::LiveStateBoundary]
    );
}
