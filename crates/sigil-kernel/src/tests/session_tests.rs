use std::{fs, io::Write};

use anyhow::Result;
use fs2::FileExt;

use crate::{
    AgentInvocationMode, AgentInvocationSource, AgentProfileCapturedEntry, AgentProfileId,
    AgentProfilePolicyEntry, AgentProfileSnapshot, AgentProfileSnapshotId, AgentProfileSource,
    AgentProfileTrustEntry, AgentResultContinuationEntry, AgentResultContinuationStatus, AgentRole,
    AgentRunContextSnapshot, AgentThreadId, AgentThreadStartedEntry, AgentThreadStateProjection,
    AgentThreadStatus, AgentThreadStatusChangedEntry, AgentTrustState, CandidateCheck, ChangeSet,
    ChangeSetId, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk, CheckCommand,
    CheckDiscoverySource, CheckPromotion, CheckSpec, CheckSpecRecordedEntry,
    ChildVerificationReceiptLinked, CompactionRecord, CompletionCriteria, ContextBodyRef,
    ContextInclusionReason, ContextItem, ContextSensitivity, ContextSource, ContextTrustLevel,
    ConversationInputKind, ConversationInputQueueControlAction, ConversationInputQueueControlEntry,
    ConversationInputQueueId, ConversationInputQueuedEntry, ConversationInputStatus,
    ConversationInputStatusEntry, ConversationInputTarget, DomainEvent, DomainPayload,
    DurableEventType, EventClass, EvidenceReceipt, EvidenceScope, ExecutionMutationProfile,
    LegacyEvent, MAX_EVENT_BYTES, McpElicitationDecision, McpElicitationEntry, MemoryConfig,
    MemoryLoadReport, MemorySnapshot, MutationEventRecorder, PlanApprovalExpiry,
    PlanApprovalPermission, PlanApprovalScope, PlanApprovedEntry, PluginCapability,
    PluginManifestSnapshot, PluginTrustDecision, PluginTrustEntry, ProjectionCursor,
    ProviderContinuationState, ReadinessEvaluatedEntry, ReadinessEvaluation, ReceiptStatus,
    RedactionState, RequiredAction, ResponseHandle, RunStatus, RuntimeContextCandidates,
    SandboxProfileRequirement, SessionRef, SessionStreamRecord, SkillDescriptor,
    SkillIndexSnapshot, SkillLoadEntry, SkillRunMode, SkillSource, SkillTrustState, StoredEvent,
    TaskId, TaskMemoryV1, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus,
    TaskStateProjection, TaskStepEntry, TaskStepId, TaskStepStatus, TerminalTaskEntry,
    TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus, ToolAccess, ToolApprovalAuditAction,
    ToolApprovalEntry, ToolEffect, ToolEgressEntry, ToolExecutionEntry, ToolExecutionStatus,
    ToolPreview, ToolPreviewFile, ToolPreviewSnapshot, ToolResultMeta, ToolSubjectAudit,
    ToolSubjectKind, ToolSubjectScope, TypedDomainEvent, UsageStats, VerificationAutoRunPolicy,
    VerificationBinding, VerificationCheckRunEntry, VerificationCheckRunStatus, VerificationPolicy,
    VerificationPolicyChangedEntry, VerificationReceipt, VerificationRecordedEntry,
    VerificationScope, VerificationStateProjection, VerificationVerdict, VisibleCompletionState,
    WorkspaceMutationDetected, WorkspaceRootSnapshot, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    WorkspaceTrustRequirement, provider::ModelMessage, stable_event_hash,
};

use super::{
    CompactionConfig, ControlEntry, JsonlSessionStore, PrefixSnapshot, Session, SessionLogEntry,
    session_stats_from_entries,
};

fn request_memory_text(request: &crate::CompletionRequest) -> String {
    request
        .messages
        .iter()
        .filter_map(|message| {
            message
                .id
                .starts_with("memory:")
                .then_some(message.content.as_deref())
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn request_context_v0_messages(request: &crate::CompletionRequest) -> Vec<&ModelMessage> {
    request
        .messages
        .iter()
        .filter(|message| message.id.starts_with("context:v0:"))
        .collect()
}

fn memory_snapshot_count(entries: &[SessionLogEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(_))
            )
        })
        .count()
}

fn test_tool_execution(status: ToolExecutionStatus) -> SessionLogEntry {
    SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: format!("call-{status:?}"),
        tool_name: "read_file".to_owned(),
        status,
        duration_ms: None,
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta::default(),
        error: None,
        model_content_hash: None,
    })))
}

fn test_tool_approval(action: ToolApprovalAuditAction) -> SessionLogEntry {
    SessionLogEntry::Control(ControlEntry::ToolApproval(ToolApprovalEntry {
        action,
        call_id: "call-approval".to_owned(),
        tool_name: "read_file".to_owned(),
        access: ToolAccess::Read,
        operation: None,
        risk: None,
        subjects: Vec::new(),
        subject_zones: Vec::new(),
        policy_decision: crate::ApprovalMode::Ask,
        external_directory_required: false,
        confirmation: None,
        snapshot_required: false,
        user_decision: None,
        reason: None,
        preview_hash: None,
    }))
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

fn sample_check_spec_recorded_entry() -> CheckSpecRecordedEntry {
    let candidate = CandidateCheck {
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
    };
    let trusted = candidate
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
        trusted,
        "event-discovery",
    )
}

fn sample_verification_policy_changed_entry() -> Result<VerificationPolicyChangedEntry> {
    VerificationPolicyChangedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        sample_verification_policy(),
        "event-policy",
    )
}

fn sample_verification_recorded_entry() -> VerificationRecordedEntry {
    let check = sample_check_spec();
    VerificationRecordedEntry {
        receipt: VerificationReceipt {
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

fn sample_verification_check_run_entry() -> VerificationCheckRunEntry {
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

fn sample_readiness_evaluated_entry() -> ReadinessEvaluatedEntry {
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

fn sample_child_verification_receipt_linked() -> ChildVerificationReceiptLinked {
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

fn sample_workspace_trust_decision_entry() -> WorkspaceTrustDecisionEntry {
    WorkspaceTrustDecisionEntry {
        workspace_id: "workspace-1".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
        trust: WorkspaceTrust::Trusted,
        decided_by_event_id: Some("trust-event".to_owned()),
        reason: Some("user trusted workspace".to_owned()),
    }
}

fn test_task_id() -> TaskId {
    TaskId::new("task-1").expect("valid task id")
}

fn test_step_id() -> TaskStepId {
    TaskStepId::new("step-1").expect("valid step id")
}

fn test_session_ref() -> SessionRef {
    SessionRef::new_relative("children/task-1.jsonl").expect("valid session ref")
}

fn test_agent_profile_id() -> AgentProfileId {
    AgentProfileId::new("explore").expect("valid agent profile id")
}

fn test_agent_profile_snapshot_id() -> AgentProfileSnapshotId {
    AgentProfileSnapshotId::new("explore-snapshot-1").expect("valid agent profile snapshot id")
}

fn test_agent_thread_id() -> AgentThreadId {
    AgentThreadId::new("thread-1").expect("valid agent thread id")
}

fn test_agent_thread_session_ref() -> SessionRef {
    SessionRef::new_relative("children/thread-1.jsonl").expect("valid agent thread session ref")
}

fn test_agent_run_context() -> AgentRunContextSnapshot {
    AgentRunContextSnapshot {
        profile_snapshot_id: test_agent_profile_snapshot_id(),
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-flash".to_owned(),
        reasoning_effort: None,
        workspace_root: WorkspaceRootSnapshot::new("/workspace").expect("valid workspace root"),
        effective_tool_scope_hash: "tool-scope-hash".to_owned(),
        effective_permission_policy_hash: "permission-policy-hash".to_owned(),
        effective_mcp_scope_hash: "mcp-scope-hash".to_owned(),
        provider_capability_hash: "provider-capability-hash".to_owned(),
        model_visible_agent_index_hash: Some("agent-index-hash".to_owned()),
        budget_policy_hash: "budget-policy-hash".to_owned(),
        provider_background_handle_ref: None,
    }
}

fn test_agent_thread_started_entry() -> AgentThreadStartedEntry {
    AgentThreadStartedEntry {
        thread_id: test_agent_thread_id(),
        parent_thread_id: None,
        parent_session_ref: test_session_ref(),
        thread_session_ref: test_agent_thread_session_ref(),
        profile_id: test_agent_profile_id(),
        profile_snapshot_id: test_agent_profile_snapshot_id(),
        run_context: test_agent_run_context(),
        objective: "inspect durable projection".to_owned(),
        prompt_hash: "prompt-hash".to_owned(),
        invocation_mode: AgentInvocationMode::Background,
        invocation_source: AgentInvocationSource::Task,
        display_name: Some("Explore".to_owned()),
        created_at_ms: Some(1),
    }
}

#[test]
fn jsonl_session_store_reads_legacy_v2_and_mixed_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::User(ModelMessage::user("legacy"));
    fs::write(
        &path,
        format!("\n{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let v2_entry =
        SessionLogEntry::Assistant(ModelMessage::assistant(Some("v2".to_owned()), Vec::new()));
    let stored = store.append_session_entry_event(&v2_entry)?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(records.len(), 2);
    assert!(matches!(records[0], SessionStreamRecord::Legacy { .. }));
    assert!(matches!(records[1], SessionStreamRecord::Stored(_)));
    assert_eq!(stored.stream_sequence, 2);

    let entries = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(entries.len(), 2);
    assert!(matches!(entries[0], SessionLogEntry::User(_)));
    assert!(matches!(entries[1], SessionLogEntry::Assistant(_)));
    Ok(())
}

#[test]
fn session_stream_records_replay_to_domain_events_for_projection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::User(ModelMessage::user("legacy"));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("v2".to_owned()),
        Vec::new(),
    )))?;
    store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "pure-durable"}),
    )?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    let domain_events = records
        .iter()
        .map(SessionStreamRecord::domain_event_record)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    assert_eq!(domain_events.len(), 3);
    assert!(matches!(domain_events[0].event, DomainEvent::Legacy(_)));
    assert!(matches!(
        domain_events[1].event,
        DomainEvent::AssistantMessageRecorded(_)
    ));
    assert!(matches!(
        domain_events[2].event,
        DomainEvent::ToolExecutionStarted(_)
    ));
    assert_eq!(domain_events[0].cursor.last_applied_stream_sequence, 1);
    assert_eq!(domain_events[1].cursor.last_applied_stream_sequence, 2);
    assert_eq!(domain_events[2].cursor.last_applied_stream_sequence, 3);

    let projected_entries = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(projected_entries.len(), 2);
    assert!(matches!(projected_entries[0], SessionLogEntry::User(_)));
    assert!(matches!(
        projected_entries[1],
        SessionLogEntry::Assistant(_)
    ));
    Ok(())
}

#[test]
fn session_next_stream_sequence_hint_counts_durable_only_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        serde_json::json!({
            "operation_id": "op-1",
            "tool_name": "mcp_server:docs",
            "tool_effect": "unknown",
            "workspace_id": "workspace-1",
            "scope_hash": "scope-main",
            "base_workspace_revision": 0,
            "workspace_revision": 1,
            "reason": "declared_write_effect",
            "unknown_dirty": true
        }),
    )?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    assert_eq!(session.entries().len(), 0);
    assert_eq!(session.next_stream_sequence_hint()?, 2);
    Ok(())
}

#[test]
fn jsonl_session_store_append_writes_v2_session_entries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;

    store.append(&SessionLogEntry::User(ModelMessage::user("first")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("second".to_owned()),
        Vec::new(),
    )))?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| matches!(record, SessionStreamRecord::Stored(_)))
    );
    assert_eq!(records[0].stream_sequence(), 1);
    assert_eq!(records[1].stream_sequence(), 2);

    let entries = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(entries.len(), 2);
    assert!(matches!(entries[0], SessionLogEntry::User(_)));
    assert!(matches!(entries[1], SessionLogEntry::Assistant(_)));
    Ok(())
}

#[test]
fn session_domain_event_helpers_cover_legacy_and_durable_only_events() -> Result<()> {
    let legacy_entry = SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("legacy answer".to_owned()),
        Vec::new(),
    ));
    let legacy = DomainEvent::Legacy(LegacyEvent {
        event_id: "legacy-event".to_owned(),
        session_id: "session-1".to_owned(),
        stream_sequence: 1,
        raw_line_hash: "sha256:legacy".to_owned(),
        payload: serde_json::to_value(&legacy_entry)?,
    });
    let restored = super::session_entry_from_domain_event(&legacy)?;
    match restored {
        Some(SessionLogEntry::Assistant(message)) => {
            assert_eq!(message.content.as_deref(), Some("legacy answer"));
        }
        other => panic!("expected legacy assistant entry, got {other:?}"),
    }

    let durable_only = DomainEvent::ToolExecutionStarted(DomainPayload {
        event_version: 1,
        payload: serde_json::json!({ "call_id": "call-1" }),
    });
    assert!(super::session_entry_from_domain_event(&durable_only)?.is_none());

    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let durable = store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({ "call_id": "call-1" }),
    )?;
    assert!(super::session_entry_from_stored_event(&durable)?.is_none());
    Ok(())
}

#[test]
fn in_memory_session_durable_event_append_is_noop() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let appended = session.append_durable_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({ "call_id": "call-1" }),
    )?;

    assert!(appended.is_none());
    assert!(session.entries().is_empty());
    Ok(())
}

#[test]
fn store_backed_session_appends_durable_only_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());

    let appended = session.append_durable_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({ "status": "running" }),
    )?;

    assert_eq!(
        appended.as_ref().map(|event| event.event_type.as_str()),
        Some(DurableEventType::RunStatusChanged.as_str())
    );
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(records.len(), 1);
    assert!(JsonlSessionStore::read_entries(store.path())?.is_empty());
    Ok(())
}

#[test]
fn session_entry_projection_applies_and_ignores_idempotent_cursor() -> Result<()> {
    let mut projection = super::SessionEntryProjection::default();
    let entry = SessionLogEntry::User(ModelMessage::user("hello"));
    let cursor = ProjectionCursor {
        session_id: "session-1".to_owned(),
        projection_schema_version: super::SESSION_ENTRY_PROJECTION_SCHEMA_VERSION,
        last_applied_stream_sequence: 1,
        last_applied_event_id: "event-1".to_owned(),
        last_applied_record_checksum: "sha256:event-1".to_owned(),
    };
    let event = DomainEvent::UserMessageRecorded(DomainPayload {
        event_version: 1,
        payload: serde_json::json!({ "session_log_entry": entry }),
    });

    projection.apply_cursor_and_event(cursor.clone(), Some(&event))?;
    assert_eq!(projection.entries.len(), 1);
    projection.apply_cursor_and_event(cursor, Some(&event))?;
    assert_eq!(projection.entries.len(), 1);

    let next_cursor = ProjectionCursor {
        session_id: "session-1".to_owned(),
        projection_schema_version: super::SESSION_ENTRY_PROJECTION_SCHEMA_VERSION,
        last_applied_stream_sequence: 2,
        last_applied_event_id: "event-2".to_owned(),
        last_applied_record_checksum: "sha256:event-2".to_owned(),
    };
    projection.apply_cursor_and_event(next_cursor, None)?;
    assert_eq!(projection.entries.len(), 1);
    Ok(())
}

#[test]
fn append_session_entry_event_maps_tool_result_and_context_classes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;

    let tool_entry = SessionLogEntry::ToolResult(ModelMessage::tool("call-1", "ok"));
    let context_entry =
        SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
            materialized_text: "prefix".to_owned(),
            sha256: "sha256:prefix".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            memory_fingerprint: "memory".to_owned(),
            tool_schema_fingerprint: "tools".to_owned(),
            skill_index_fingerprint: "skills".to_owned(),
        }));
    let tool_event = store.append_session_entry_event(&tool_entry)?;
    let context_event = store.append_session_entry_event(&context_entry)?;

    assert_eq!(
        tool_event.event_type,
        DurableEventType::ToolResultRecorded.as_str()
    );
    assert_eq!(tool_event.event_class, EventClass::Critical);
    assert_eq!(
        context_event.event_type,
        DurableEventType::ContextSourceCaptured.as_str()
    );
    assert_eq!(context_event.event_class, EventClass::NonCritical);
    Ok(())
}

#[test]
fn session_private_helpers_cover_identity_messages_tail_and_event_mapping() -> Result<()> {
    let identity = SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
    });
    let non_identity_control =
        SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats::default()));
    let user = SessionLogEntry::User(ModelMessage::user("prompt"));
    let assistant = SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("answer".to_owned()),
        Vec::new(),
    ));
    let tool = SessionLogEntry::ToolResult(ModelMessage::tool("call-1", "ok"));

    assert!(super::is_session_identity_entry(&identity));
    assert!(!super::is_session_identity_entry(&non_identity_control));
    assert!(!super::is_session_identity_entry(&user));
    assert_eq!(
        super::stream_sequence_mismatch_message(4, 8, 4),
        "stream_sequence does not match expected sequence on line 4: 8 vs 4"
    );
    assert_eq!(
        super::stream_session_mismatch_message(5, "session-b", "session-a"),
        "session_id does not match stream session_id on line 5: session-b vs session-a"
    );
    assert_eq!(
        super::session_entry_event_type(&user),
        DurableEventType::UserMessageRecorded
    );
    assert_eq!(
        super::session_entry_event_type(&assistant),
        DurableEventType::AssistantMessageRecorded
    );
    assert_eq!(
        super::session_entry_event_type(&tool),
        DurableEventType::ToolResultRecorded
    );
    assert_eq!(
        super::session_entry_event_type(&test_tool_approval(ToolApprovalAuditAction::Resolved)),
        DurableEventType::ApprovalResolved
    );
    assert_eq!(
        super::session_entry_event_type(&non_identity_control),
        DurableEventType::SessionEntryRecorded
    );
    assert_eq!(
        super::session_entry_event_class(DurableEventType::ContextSourceCaptured),
        EventClass::NonCritical
    );
    assert_eq!(
        super::session_entry_event_class(DurableEventType::SessionEntryRecorded),
        EventClass::NonCritical
    );
    assert_eq!(
        super::session_entry_event_class(DurableEventType::ToolExecutionStarted),
        EventClass::Critical
    );
    assert_eq!(
        super::tool_execution_event_type(ToolExecutionStatus::Started),
        DurableEventType::ToolExecutionStarted
    );
    assert_eq!(
        super::tool_execution_event_type(ToolExecutionStatus::Failed),
        DurableEventType::ToolExecutionFinished
    );

    let mut expected_session_id = None;
    let stored = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-helper".to_owned(),
        "session-helper".to_owned(),
        1,
        serde_json::json!({ "call_id": "helper" }),
    )?;
    super::validate_stream_record_identity(
        1,
        1,
        &stored.session_id,
        stored.stream_sequence,
        &mut expected_session_id,
    )?;
    assert_eq!(expected_session_id.as_deref(), Some("session-helper"));

    let mut expected_session_id = None;
    super::validate_stream_record_identity(1, 1, "session-legacy", 1, &mut expected_session_id)?;
    assert_eq!(expected_session_id.as_deref(), Some("session-legacy"));
    Ok(())
}

#[test]
fn session_entry_from_json_line_decodes_legacy_v2_and_skips_unknown_noncritical() -> Result<()> {
    assert!(JsonlSessionStore::session_entry_from_json_line("  \n  ")?.is_none());

    let legacy_entry = SessionLogEntry::User(ModelMessage::user("legacy"));
    let legacy_line = serde_json::to_string(&legacy_entry)?;
    let decoded = JsonlSessionStore::session_entry_from_json_line(&legacy_line)?
        .expect("legacy line should decode to session entry");
    assert!(matches!(decoded, SessionLogEntry::User(_)));

    let v2_entry =
        SessionLogEntry::Assistant(ModelMessage::assistant(Some("v2".to_owned()), Vec::new()));
    let stored = StoredEvent::new(
        DurableEventType::AssistantMessageRecorded,
        EventClass::Critical,
        "event-assistant".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({ "session_log_entry": v2_entry }),
    )?;
    let decoded = JsonlSessionStore::session_entry_from_json_line(&stored.to_json_line()?)?
        .expect("stored event should decode to session entry");
    assert!(matches!(decoded, SessionLogEntry::Assistant(_)));

    let future = StoredEvent::new_raw(
        "future_noncritical_event",
        EventClass::NonCritical,
        "event-future".to_owned(),
        "session-1".to_owned(),
        2,
        serde_json::json!({ "session_log_entry": legacy_entry }),
    )?;
    assert!(JsonlSessionStore::session_entry_from_json_line(&future.to_json_line()?)?.is_none());

    Ok(())
}

#[test]
fn append_event_handles_blank_log_with_fast_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(&path, "\n   \r\n")?;
    let store = JsonlSessionStore::new(&path)?;

    let event = store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "blank-log"}),
    )?;

    assert_eq!(event.stream_sequence, 1);
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].stream_sequence(), 1);
    Ok(())
}

#[test]
fn load_from_v2_only_store_persists_identity_as_v2_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "call-1"}),
    )?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
        )
    }));
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| matches!(record, SessionStreamRecord::Stored(_)))
    );
    assert_eq!(records[0].stream_sequence(), 1);
    assert_eq!(records[1].stream_sequence(), 2);
    Ok(())
}

#[test]
fn load_from_store_keeps_existing_identity_without_duplicate_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
    }))?;

    let session = Session::load_from_store("other-provider", "other-model", store)?;

    let identity_count = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            )
        })
        .count();
    assert_eq!(identity_count, 1);
    assert_eq!(session.provider_name(), "deepseek");
    assert_eq!(session.model_name(), "deepseek-v4-flash");

    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(records.len(), 1);
    Ok(())
}

#[test]
fn read_entries_skips_v2_events_without_session_log_entry_payload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "call-1"}),
    )?;

    let entries = JsonlSessionStore::read_entries(&path)?;

    assert!(entries.is_empty());
    Ok(())
}

#[test]
fn read_entries_skips_unknown_noncritical_session_log_entry_payload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let entry = SessionLogEntry::User(ModelMessage::user("should-not-replay"));
    let event = StoredEvent::new_raw(
        "future_noncritical_event",
        EventClass::NonCritical,
        "event-future".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({ "session_log_entry": entry }),
    )?;
    fs::write(&path, event.to_json_line()?)?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("known event after ignored unknown".to_owned()),
        Vec::new(),
    )))?;

    let entries = JsonlSessionStore::read_entries(&path)?;

    assert_eq!(entries.len(), 1);
    assert!(matches!(entries[0], SessionLogEntry::Assistant(_)));
    Ok(())
}

#[test]
fn session_entry_event_type_maps_session_entries_to_durable_types() -> Result<()> {
    let change_set_id = ChangeSetId::new("change-1")?;
    let cases = vec![
        (
            SessionLogEntry::User(ModelMessage::user("hello")),
            DurableEventType::UserMessageRecorded,
        ),
        (
            SessionLogEntry::Assistant(ModelMessage::assistant(
                Some("answer".to_owned()),
                Vec::new(),
            )),
            DurableEventType::AssistantMessageRecorded,
        ),
        (
            SessionLogEntry::ToolResult(ModelMessage::tool("call-1", "ok")),
            DurableEventType::ToolResultRecorded,
        ),
        (
            test_tool_approval(ToolApprovalAuditAction::Requested),
            DurableEventType::SessionEntryRecorded,
        ),
        (
            test_tool_approval(ToolApprovalAuditAction::Resolved),
            DurableEventType::ApprovalResolved,
        ),
        (
            test_tool_execution(ToolExecutionStatus::Started),
            DurableEventType::ToolExecutionStarted,
        ),
        (
            test_tool_execution(ToolExecutionStatus::Completed),
            DurableEventType::ToolExecutionFinished,
        ),
        (
            test_tool_execution(ToolExecutionStatus::Failed),
            DurableEventType::ToolExecutionFinished,
        ),
        (
            test_tool_execution(ToolExecutionStatus::Cancelled),
            DurableEventType::ToolExecutionFinished,
        ),
        (
            test_tool_execution(ToolExecutionStatus::Interrupted),
            DurableEventType::ToolExecutionFinished,
        ),
        (
            SessionLogEntry::Control(ControlEntry::ToolEgress(Box::new(ToolEgressEntry {
                call_id: "call-egress".to_owned(),
                tool_name: "mcp__server__tool".to_owned(),
                destination: "server".to_owned(),
                operation: "tools/call".to_owned(),
                subjects: Vec::new(),
                payload: serde_json::json!({"redacted": true}),
                redacted: true,
            }))),
            DurableEventType::EgressDecisionRecorded,
        ),
        (
            SessionLogEntry::Control(ControlEntry::PluginTrustDecision(PluginTrustEntry {
                plugin_id: "repo-review".to_owned(),
                manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
                manifest_hash:
                    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_owned(),
                manifest_version: None,
                capability_digest: None,
                decision: PluginTrustDecision::Trusted,
                reviewed_at_ms: 42,
            })),
            DurableEventType::ExtensionTrustDecision,
        ),
        (
            SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(
                AgentProfileTrustEntry {
                    profile_id: AgentProfileId::new("explore")?,
                    source: AgentProfileSource::Workspace,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    decision: AgentTrustState::Trusted,
                    reviewed_at_ms: 43,
                },
            )),
            DurableEventType::ExtensionTrustDecision,
        ),
        (
            SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: test_task_id(),
                parent_session_ref: test_session_ref(),
                objective: "implement events".to_owned(),
                status: TaskRunStatus::Running,
                reason: None,
            })),
            DurableEventType::TaskStatusChanged,
        ),
        (
            SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: test_task_id(),
                plan_version: 1,
                status: TaskPlanStatus::Accepted,
                steps: Vec::new(),
                reason: None,
            })),
            DurableEventType::TaskStatusChanged,
        ),
        (
            SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
                task_id: test_task_id(),
                plan_version: 1,
                step_id: test_step_id(),
                role: AgentRole::Executor,
                status: TaskStepStatus::Running,
                title: Some("implement".to_owned()),
                summary: None,
                reason: None,
            })),
            DurableEventType::TaskStatusChanged,
        ),
        (
            SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(
                sample_check_spec_recorded_entry(),
            )),
            DurableEventType::CheckSpecRecorded,
        ),
        (
            SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(
                sample_verification_policy_changed_entry()?,
            )),
            DurableEventType::VerificationPolicyChanged,
        ),
        (
            SessionLogEntry::Control(ControlEntry::VerificationCheckRun(
                sample_verification_check_run_entry(),
            )),
            DurableEventType::VerificationCheckRun,
        ),
        (
            SessionLogEntry::Control(ControlEntry::VerificationRecorded(
                sample_verification_recorded_entry(),
            )),
            DurableEventType::VerificationRecorded,
        ),
        (
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
                sample_readiness_evaluated_entry(),
            )),
            DurableEventType::ReadinessEvaluated,
        ),
        (
            SessionLogEntry::Control(ControlEntry::ChildVerificationReceiptLinked(
                sample_child_verification_receipt_linked(),
            )),
            DurableEventType::ChildVerificationReceiptLinked,
        ),
        (
            SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(
                sample_workspace_trust_decision_entry(),
            )),
            DurableEventType::WorkspaceTrustDecision,
        ),
        (
            SessionLogEntry::Control(ControlEntry::ChangeSetApplied(ChangeSetResult {
                id: change_set_id,
                status: ChangeSetResultStatus::Applied,
                file_results: Vec::new(),
                message: None,
            })),
            DurableEventType::SessionEntryRecorded,
        ),
        (
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats::default())),
            DurableEventType::SessionEntryRecorded,
        ),
        (
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
                materialized_text: "prefix".to_owned(),
                sha256: "sha256:prefix".to_owned(),
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
                memory_fingerprint: "memory".to_owned(),
                tool_schema_fingerprint: "tools".to_owned(),
                skill_index_fingerprint: "skills".to_owned(),
            })),
            DurableEventType::ContextSourceCaptured,
        ),
        (
            SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(MemorySnapshot {
                messages: Vec::new(),
                report: MemoryLoadReport::default(),
            })),
            DurableEventType::ContextSourceCaptured,
        ),
        (
            SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(SkillIndexSnapshot::new(
                vec![SkillDescriptor {
                    id: "review".to_owned(),
                    name: "Review".to_owned(),
                    description: "Review code".to_owned(),
                    when_to_use: Some("Use for review.".to_owned()),
                    root: "skills/review".into(),
                    entrypoint: "skills/review/SKILL.md".into(),
                    source: SkillSource::Workspace,
                    sha256: "sha256:skill".to_owned(),
                    enabled: true,
                    trust: SkillTrustState::Trusted,
                    model_invocable: true,
                    user_invocable: true,
                    run_as: SkillRunMode::Inline,
                    agent: None,
                    argument_hint: None,
                    allowed_tools: Default::default(),
                    disallowed_tools: Default::default(),
                    path_patterns: Vec::new(),
                }],
            )?)),
            DurableEventType::ContextSourceCaptured,
        ),
        (
            SessionLogEntry::Control(ControlEntry::SkillLoaded(SkillLoadEntry {
                skill_id: "review".to_owned(),
                sha256: "sha256:skill".to_owned(),
                source: SkillSource::Workspace,
                entrypoint: "skills/review/SKILL.md".into(),
                run_id: Some("run-1".to_owned()),
                call_id: Some("call-1".to_owned()),
                byte_count: 128,
                line_count: 7,
                loaded_at_ms: 44,
            })),
            DurableEventType::ContextSourceCaptured,
        ),
        (
            SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(
                PluginManifestSnapshot {
                    plugin_id: "repo-review".to_owned(),
                    name: "Repository Review".to_owned(),
                    version: "0.1.0".to_owned(),
                    description: None,
                    manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
                    manifest_hash:
                        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                            .to_owned(),
                    capabilities: vec![PluginCapability::Skill {
                        path: "skills/review/SKILL.md".into(),
                    }],
                    trust: PluginTrustDecision::NeedsReview,
                },
            )),
            DurableEventType::ContextSourceCaptured,
        ),
        (
            SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
                AgentProfileCapturedEntry {
                    snapshot: AgentProfileSnapshot {
                        snapshot_id: AgentProfileSnapshotId::new("snapshot-1")?,
                        profile_id: AgentProfileId::new("explore")?,
                        source: AgentProfileSource::Workspace,
                        source_hash: "sha256:source".to_owned(),
                        profile_hash: "sha256:profile".to_owned(),
                        resolved_tool_scope_hash: "sha256:tools".to_owned(),
                        resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                        resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                        resolved_skill_hashes: Vec::new(),
                        trust_state: AgentTrustState::Trusted,
                    },
                },
            )),
            DurableEventType::ContextSourceCaptured,
        ),
        (
            SessionLogEntry::Control(ControlEntry::Note {
                kind: "note".to_owned(),
                data: serde_json::json!({"value": 1}),
            }),
            DurableEventType::SessionEntryRecorded,
        ),
    ];

    for (entry, expected) in cases {
        assert_eq!(super::session_entry_event_type(&entry), expected);
    }
    Ok(())
}

#[test]
fn append_session_entry_event_uses_noncritical_class_for_compatibility_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;

    let note = store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "note".to_owned(),
        data: serde_json::json!({"value": 1}),
    }))?;
    let context = store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
            materialized_text: "prefix".to_owned(),
            sha256: "sha256:prefix".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            memory_fingerprint: "memory".to_owned(),
            tool_schema_fingerprint: "tools".to_owned(),
            skill_index_fingerprint: "skills".to_owned(),
        }),
    ))?;
    let user =
        store.append_session_entry_event(&SessionLogEntry::User(ModelMessage::user("hi")))?;

    assert_eq!(
        note.event_type,
        DurableEventType::SessionEntryRecorded.as_str()
    );
    assert_eq!(note.event_class, EventClass::NonCritical);
    assert_eq!(
        context.event_type,
        DurableEventType::ContextSourceCaptured.as_str()
    );
    assert_eq!(context.event_class, EventClass::NonCritical);
    assert_eq!(
        user.event_type,
        DurableEventType::UserMessageRecorded.as_str()
    );
    assert_eq!(user.event_class, EventClass::Critical);
    Ok(())
}

#[test]
fn append_event_rejects_non_appendable_legacy_event_type() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;

    let error = store
        .append_event(
            DurableEventType::Legacy,
            EventClass::Critical,
            serde_json::json!({}),
        )
        .expect_err("legacy event type should not be appendable");

    assert!(error.to_string().contains("cannot be appended"));
    Ok(())
}

#[test]
fn append_event_rejects_mismatched_known_event_class() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;

    let error = store
        .append_event(
            DurableEventType::ToolExecutionStarted,
            EventClass::NonCritical,
            serde_json::json!({"call_id": "call-1"}),
        )
        .expect_err("recovery-critical event must not be appended as non-critical");

    assert!(error.to_string().contains("event_class must be"));
    Ok(())
}

#[test]
fn append_event_fails_when_session_file_is_locked() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let locked_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    locked_file.try_lock_exclusive()?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .append_event(
            DurableEventType::ToolExecutionStarted,
            EventClass::Critical,
            serde_json::json!({"call_id": "call-1"}),
        )
        .expect_err("second writer should fail while file lock is held");

    locked_file.unlock()?;
    assert!(error.to_string().contains("failed to lock"));
    Ok(())
}

#[test]
fn read_event_records_fails_when_session_file_is_exclusively_locked() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(&path, "{}\n")?;
    let locked_file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
    locked_file.try_lock_exclusive()?;

    let error = JsonlSessionStore::read_event_records(&path)
        .expect_err("shared reader should fail while exclusive file lock is held");

    locked_file.unlock()?;
    assert!(error.to_string().contains("failed to lock"));
    Ok(())
}

#[test]
fn writer_mode_loader_fails_when_session_file_is_locked() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(&path, "{}\n")?;
    let locked_file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
    locked_file.try_lock_exclusive()?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .read_event_records_writer()
        .expect_err("writer-mode reader should fail while file lock is held");

    locked_file.unlock()?;
    assert!(error.to_string().contains("failed to lock"));
    Ok(())
}

#[test]
fn legacy_ids_and_session_id_are_stable_after_v2_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(
        &path,
        format!(
            "\n{}\n{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("one")))?,
            serde_json::to_string(&SessionLogEntry::Assistant(ModelMessage::assistant(
                Some("two".to_owned()),
                Vec::new(),
            )))?,
        ),
    )?;

    let before = JsonlSessionStore::read_event_records(&path)?;
    let legacy_ids_before = before
        .iter()
        .map(|record| match record {
            SessionStreamRecord::Legacy { event, .. } => {
                (event.session_id.clone(), event.event_id.clone())
            }
            SessionStreamRecord::Stored(_) => unreachable!("only legacy before append"),
        })
        .collect::<Vec<_>>();

    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    let after = JsonlSessionStore::read_event_records(&path)?;
    let legacy_ids_after = after
        .iter()
        .take(2)
        .map(|record| match record {
            SessionStreamRecord::Legacy { event, .. } => {
                (event.session_id.clone(), event.event_id.clone())
            }
            SessionStreamRecord::Stored(_) => unreachable!("legacy prefix should remain legacy"),
        })
        .collect::<Vec<_>>();

    assert_eq!(legacy_ids_after, legacy_ids_before);
    assert!(matches!(after[2], SessionStreamRecord::Stored(_)));
    assert_eq!(after[2].stream_sequence(), 3);
    Ok(())
}

#[test]
fn legacy_after_v2_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&SessionLogEntry::User(ModelMessage::user(
            "legacy after v2"
        )))?
    )?;

    let error = JsonlSessionStore::read_event_records(&path)
        .expect_err("legacy after v2 should fail closed");
    assert!(
        error
            .to_string()
            .contains("legacy session entry appears after v2")
    );
    Ok(())
}

#[test]
fn append_event_assigns_local_sequence_without_global_ordering() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let first_store = JsonlSessionStore::new(temp.path().join("first.jsonl"))?;
    let second_store = JsonlSessionStore::new(temp.path().join("second.jsonl"))?;

    let first = first_store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "first-1"}),
    )?;
    let second = first_store.append_event(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        serde_json::json!({"call_id": "first-1"}),
    )?;
    let other = second_store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "second-1"}),
    )?;

    assert_eq!(first.stream_sequence, 1);
    assert_eq!(second.stream_sequence, 2);
    assert_eq!(other.stream_sequence, 1);
    assert_ne!(first.session_id, other.session_id);
    Ok(())
}

#[test]
fn append_event_reconciles_pending_tail_recovery_intent_before_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("ok")))?;
    fs::write(&path, format!("{valid}\n"))?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let session_id = records[0].session_id().to_owned();
    let recovered_size = fs::metadata(&path)?.len();
    let corrupt_content = format!("{}{{bad-tail", fs::read_to_string(&path)?);
    fs::write(&path, &corrupt_content)?;
    super::write_tail_recovery_intent(
        &path,
        &super::TailRecoveryIntent {
            original_size: corrupt_content.len() as u64,
            recovered_size,
            discarded_bytes: corrupt_content.len() as u64 - recovered_size,
            quarantine_path: temp.path().join("quarantined-copy"),
            original_hash: stable_event_hash(corrupt_content.as_bytes()),
            event_id: "tail-recovery-event".to_owned(),
            session_id,
        },
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let appended = store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "after-recovery"}),
    )?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::LogTailRecovered.as_str()
                    && event.event_id == "tail-recovery-event"
        )
    }));
    assert_eq!(appended.stream_sequence, 3);
    assert_eq!(
        records.last().map(SessionStreamRecord::stream_sequence),
        Some(3)
    );
    assert!(!super::tail_recovery_intent_path(&path).exists());
    Ok(())
}

#[test]
fn append_event_fallback_rejects_non_appendable_event_type() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::User(ModelMessage::user("legacy"));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .append_event(
            DurableEventType::Legacy,
            EventClass::Critical,
            serde_json::json!({}),
        )
        .expect_err("legacy event type is not appendable");

    assert!(
        error
            .to_string()
            .contains("cannot be appended as a v2 event")
    );
    Ok(())
}

#[test]
fn append_event_recovers_invalid_v2_tail_before_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "first"}),
    )?;
    let invalid_tail = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-invalid-tail".to_owned(),
        "session-invalid".to_owned(),
        2,
        serde_json::json!({"call_id": "bad"}),
    )?;
    let mut invalid_tail = serde_json::to_string(&invalid_tail)?;
    invalid_tail.truncate(invalid_tail.len() - 1);
    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    file.write_all(invalid_tail.as_bytes())?;

    let appended = store.append_event(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        serde_json::json!({"call_id": "first"}),
    )?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::LogTailRecovered.as_str()
        )
    }));
    assert_eq!(appended.stream_sequence, 3);
    assert_eq!(
        records.last().map(SessionStreamRecord::stream_sequence),
        Some(3)
    );
    Ok(())
}

#[test]
fn append_event_recovers_oversized_tail_record() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("prefix")))?;
    fs::write(
        &path,
        format!("{valid}\n{}", "x".repeat(MAX_EVENT_BYTES + 64 * 1024)),
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let appended = store.append_event(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        serde_json::json!({"call_id": "too-large"}),
    )?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::LogTailRecovered.as_str()
        )
    }));
    assert_eq!(appended.stream_sequence, 3);
    assert_eq!(
        records.last().map(SessionStreamRecord::stream_sequence),
        Some(3)
    );
    Ok(())
}

#[test]
fn read_only_loader_does_not_recover_tail_corruption() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("ok")))?;
    fs::write(&path, format!("{valid}\n{{bad-tail"))?;
    let before = fs::read_to_string(&path)?;

    let error =
        JsonlSessionStore::read_event_records(&path).expect_err("read-only load should fail");
    let after = fs::read_to_string(&path)?;

    assert!(error.to_string().contains("failed to parse session entry"));
    assert_eq!(after, before);
    assert!(!temp.path().join(".sigil-recovery").exists());
    Ok(())
}

#[test]
fn load_from_store_recovers_tail_corruption_with_audit_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("ok")))?;
    fs::write(&path, format!("{valid}\n{{bad-tail"))?;
    let store = JsonlSessionStore::new(&path)?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
        )
    }));
    let content = fs::read_to_string(&path)?;
    assert!(!content.contains("bad-tail"));
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::LogTailRecovered.as_str()
        )
    }));
    Ok(())
}

#[test]
fn writer_mode_loader_recovers_tail_corruption_once() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("ok")))?;
    fs::write(&path, format!("{valid}\n{{bad-tail"))?;
    let store = JsonlSessionStore::new(&path)?;

    let records = store.read_event_records_writer()?;
    let recovery_count = records
        .iter()
        .filter(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type == DurableEventType::LogTailRecovered.as_str()
            )
        })
        .count();
    assert_eq!(recovery_count, 1);
    assert!(temp.path().join(".sigil-recovery").exists());
    assert!(!fs::read_to_string(&path)?.contains("bad-tail"));

    let second = store.read_event_records_writer()?;
    let second_recovery_count = second
        .iter()
        .filter(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type == DurableEventType::LogTailRecovered.as_str()
            )
        })
        .count();
    assert_eq!(second_recovery_count, 1);
    Ok(())
}

#[test]
fn writer_mode_loader_recovers_tail_corruption_without_prior_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(&path, "{bad-tail")?;
    let store = JsonlSessionStore::new(&path)?;

    let records = store.read_event_records_writer()?;

    assert_eq!(records.len(), 1);
    let SessionStreamRecord::Stored(event) = &records[0] else {
        panic!("tail recovery should append a stored event");
    };
    assert_eq!(
        event.event_type,
        DurableEventType::LogTailRecovered.as_str()
    );
    assert_eq!(event.stream_sequence, 1);
    assert!(
        event.payload["discarded_bytes"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert!(!fs::read_to_string(&path)?.contains("bad-tail"));
    assert!(temp.path().join(".sigil-recovery").exists());
    Ok(())
}

#[test]
fn writer_mode_loader_clears_completed_tail_recovery_intent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::User(ModelMessage::user("ok"));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let session_id = records[0].session_id().to_owned();
    let recovered_size = fs::metadata(&path)?.len();
    let recovery_event = StoredEvent::new(
        DurableEventType::LogTailRecovered,
        EventClass::Critical,
        "tail-recovery-event".to_owned(),
        session_id.clone(),
        2,
        serde_json::json!({
            "original_size": recovered_size + 9,
            "recovered_size": recovered_size,
            "discarded_bytes": 9,
            "quarantine_path": temp.path().join("quarantined-copy"),
            "original_hash": "sha256:original",
        }),
    )?;
    let mut file = fs::OpenOptions::new().append(true).open(&path)?;
    file.write_all(recovery_event.to_json_line()?.as_bytes())?;
    super::write_tail_recovery_intent(
        &path,
        &super::TailRecoveryIntent {
            original_size: recovered_size + 9,
            recovered_size,
            discarded_bytes: 9,
            quarantine_path: temp.path().join("quarantined-copy"),
            original_hash: "sha256:original".to_owned(),
            event_id: "tail-recovery-event".to_owned(),
            session_id,
        },
    )?;
    let before_size = fs::metadata(&path)?.len();
    let store = JsonlSessionStore::new(&path)?;

    let records = store.read_event_records_writer()?;

    assert_eq!(records.len(), 2);
    assert_eq!(fs::metadata(&path)?.len(), before_size);
    assert!(!super::tail_recovery_intent_path(&path).exists());
    Ok(())
}

#[test]
fn v2_stream_sequence_gap_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-2".to_owned(),
        "session-1".to_owned(),
        2,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    fs::write(&path, event.to_json_line()?)?;

    let error = JsonlSessionStore::read_event_records(&path).expect_err("sequence gap should fail");

    assert!(
        error
            .to_string()
            .contains("does not match expected sequence")
    );
    Ok(())
}

#[test]
fn v2_stream_checksum_mismatch_fails_with_line_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    event.record_checksum = "sha256:jcs-v1:wrong".to_owned();
    fs::write(&path, format!("{}\n", serde_json::to_string(&event)?))?;

    let error = JsonlSessionStore::read_event_records(&path)
        .expect_err("checksum mismatch should fail closed");

    let message = error.to_string();
    assert!(message.contains("failed to parse stored event on line 1"));
    assert!(format!("{error:#}").contains("checksum mismatch"));
    Ok(())
}

#[test]
fn writer_mode_loader_rejects_tail_unknown_critical_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let event = StoredEvent::new_raw(
        "future_critical_event",
        EventClass::Critical,
        "event-future".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({"value": "must-not-recover"}),
    )?;
    fs::write(&path, event.to_json_line()?)?;
    let before = fs::read_to_string(&path)?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .read_event_records_writer()
        .expect_err("unknown critical tail event should fail closed");

    assert!(format!("{error:#}").contains("unknown critical event future_critical_event"));
    assert_eq!(fs::read_to_string(&path)?, before);
    assert!(!temp.path().join(".sigil-recovery").exists());
    Ok(())
}

#[test]
fn append_event_rejects_tail_checksum_mismatch_before_recovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    event.record_checksum = "sha256:jcs-v1:wrong".to_owned();
    fs::write(&path, format!("{}\n", serde_json::to_string(&event)?))?;
    let before = fs::read_to_string(&path)?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .append_event(
            DurableEventType::ToolExecutionFinished,
            EventClass::Critical,
            serde_json::json!({"call_id": "call-1"}),
        )
        .expect_err("checksum mismatch tail event should fail closed before append");

    assert!(format!("{error:#}").contains("checksum mismatch"));
    assert_eq!(fs::read_to_string(&path)?, before);
    assert!(!temp.path().join(".sigil-recovery").exists());
    Ok(())
}

#[test]
fn v2_stream_session_id_mismatch_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    let second = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-2".to_owned(),
        "session-2".to_owned(),
        2,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    fs::write(
        &path,
        format!("{}{}", first.to_json_line()?, second.to_json_line()?),
    )?;

    let error =
        JsonlSessionStore::read_event_records(&path).expect_err("session mismatch should fail");

    assert!(
        error
            .to_string()
            .contains("does not match stream session_id")
    );
    Ok(())
}

#[test]
fn writer_mode_loader_rejects_middle_corruption() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("one")))?;
    let second = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("two")))?;
    fs::write(&path, format!("{first}\nnot-json\n{second}\n"))?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .read_event_records_writer()
        .expect_err("middle corruption should fail");

    assert!(error.to_string().contains("middle corruption"));
    assert!(!temp.path().join(".sigil-recovery").exists());
    Ok(())
}

#[test]
fn append_event_rejects_middle_corruption_before_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    let third = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-3".to_owned(),
        "session-1".to_owned(),
        3,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    fs::write(
        &path,
        format!(
            "{}not-json\n{}",
            first.to_json_line()?,
            third.to_json_line()?
        ),
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .append_event(
            DurableEventType::RunFinalized,
            EventClass::Critical,
            serde_json::json!({"run_status": "completed"}),
        )
        .expect_err("append should fail closed on middle corruption");

    assert!(error.to_string().contains("middle corruption"));
    let content = fs::read_to_string(&path)?;
    assert!(!content.contains("run_finalized"));
    Ok(())
}

#[test]
fn append_event_rejects_sequence_gap_before_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    let third = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-3".to_owned(),
        "session-1".to_owned(),
        3,
        serde_json::json!({"call_id": "call-1"}),
    )?;
    fs::write(
        &path,
        format!("{}{}", first.to_json_line()?, third.to_json_line()?),
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .append_event(
            DurableEventType::RunFinalized,
            EventClass::Critical,
            serde_json::json!({"run_status": "completed"}),
        )
        .expect_err("append should fail closed on stream sequence gap");

    assert!(error.to_string().contains("stream_sequence"));
    let content = fs::read_to_string(&path)?;
    assert!(!content.contains("run_finalized"));
    Ok(())
}

#[test]
fn writer_mode_loader_finishes_tail_recovery_intent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("ok")))?;
    fs::write(&path, format!("{valid}\n"))?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let session_id = records[0].session_id().to_owned();
    let recovered_size = fs::metadata(&path)?.len();
    super::write_tail_recovery_intent(
        &path,
        &super::TailRecoveryIntent {
            original_size: recovered_size + 9,
            recovered_size,
            discarded_bytes: 9,
            quarantine_path: temp.path().join("quarantined-copy"),
            original_hash: "sha256:original".to_owned(),
            event_id: "tail-recovery-event".to_owned(),
            session_id,
        },
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let records = store.read_event_records_writer()?;

    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::LogTailRecovered.as_str()
                    && event.event_id == "tail-recovery-event"
        )
    }));
    assert!(!super::tail_recovery_intent_path(&path).exists());
    Ok(())
}

#[test]
fn writer_mode_loader_replays_tail_recovery_intent_before_truncate() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("ok")))?;
    fs::write(&path, format!("{valid}\n"))?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let session_id = records[0].session_id().to_owned();
    let recovered_size = fs::metadata(&path)?.len();
    let corrupt_content = format!("{}{{bad-tail", fs::read_to_string(&path)?);
    fs::write(&path, &corrupt_content)?;
    super::write_tail_recovery_intent(
        &path,
        &super::TailRecoveryIntent {
            original_size: corrupt_content.len() as u64,
            recovered_size,
            discarded_bytes: corrupt_content.len() as u64 - recovered_size,
            quarantine_path: temp.path().join("quarantined-copy"),
            original_hash: stable_event_hash(corrupt_content.as_bytes()),
            event_id: "tail-recovery-event".to_owned(),
            session_id,
        },
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let records = store.read_event_records_writer()?;

    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::LogTailRecovered.as_str()
                    && event.event_id == "tail-recovery-event"
        )
    }));
    assert!(!fs::read_to_string(&path)?.contains("bad-tail"));
    assert!(!super::tail_recovery_intent_path(&path).exists());
    Ok(())
}

#[test]
fn writer_mode_loader_rejects_tail_recovery_intent_hash_mismatch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let corrupt_content = "{bad-tail";
    fs::write(&path, corrupt_content)?;
    super::write_tail_recovery_intent(
        &path,
        &super::TailRecoveryIntent {
            original_size: corrupt_content.len() as u64,
            recovered_size: 0,
            discarded_bytes: corrupt_content.len() as u64,
            quarantine_path: temp.path().join("quarantined-copy"),
            original_hash: "sha256:not-the-current-log".to_owned(),
            event_id: "tail-recovery-event".to_owned(),
            session_id: "session-1".to_owned(),
        },
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .read_event_records_writer()
        .expect_err("hash mismatch should fail closed");

    assert!(format!("{error:#}").contains("hash"));
    Ok(())
}

#[test]
fn writer_mode_loader_rejects_tail_recovery_intent_past_log_length() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let corrupt_content = "{bad-tail";
    fs::write(&path, corrupt_content)?;
    super::write_tail_recovery_intent(
        &path,
        &super::TailRecoveryIntent {
            original_size: corrupt_content.len() as u64,
            recovered_size: corrupt_content.len() as u64 + 1,
            discarded_bytes: 0,
            quarantine_path: temp.path().join("quarantined-copy"),
            original_hash: stable_event_hash(corrupt_content.as_bytes()),
            event_id: "tail-recovery-event".to_owned(),
            session_id: "session-1".to_owned(),
        },
    )?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .read_event_records_writer()
        .expect_err("impossible recovered_size should fail closed");

    assert!(format!("{error:#}").contains("recovered_size"));
    Ok(())
}

#[test]
fn load_from_store_recovers_identity_from_prefix_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
            materialized_text: "prefix".to_owned(),
            sha256: "abc".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            memory_fingerprint: "none".to_owned(),
            tool_schema_fingerprint: "tools".to_owned(),
            skill_index_fingerprint: "skills".to_owned(),
        }),
    ))?;

    let session = Session::load_from_store("other-provider", "other-model", store)?;

    assert_eq!(session.provider_name(), "deepseek");
    assert_eq!(session.model_name(), "deepseek-v4-flash");
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name,
                model_name,
            }) if provider_name == "deepseek" && model_name == "deepseek-v4-flash"
        )
    }));
    Ok(())
}

#[test]
fn load_from_store_persists_identity_for_empty_log() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;

    assert_eq!(session.provider_name(), "deepseek");
    assert_eq!(session.model_name(), "deepseek-v4-flash");
    assert_eq!(session.entries().len(), 1);
    assert!(matches!(
        session.entries()[0],
        SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
    ));
    Ok(())
}

#[test]
fn tool_preview_captured_control_entry_roundtrips() -> Result<()> {
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &ToolPreview {
            title: "Write file".to_owned(),
            summary: "Create a file".to_owned(),
            body: "preview body".to_owned(),
            changed_files: vec!["README.md".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "README.md".to_owned(),
                diff: "--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+hello".to_owned(),
            }],
        },
        Default::default(),
        Some("preview-hash".to_owned()),
    );
    let entry = SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot.clone()));

    let json = serde_json::to_string(&entry)?;
    let decoded: SessionLogEntry = serde_json::from_str(&json)?;

    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(restored))
            if restored == snapshot
    ));
    Ok(())
}

#[test]
fn tool_egress_control_entry_roundtrips() -> Result<()> {
    let entry = ToolEgressEntry {
        call_id: "call-1".to_owned(),
        tool_name: "mcp__fake__echo".to_owned(),
        destination: "mcp:fake".to_owned(),
        operation: "tools/call".to_owned(),
        subjects: vec![ToolSubjectAudit {
            kind: ToolSubjectKind::McpTool,
            original: "mcp__fake__echo".to_owned(),
            normalized: "mcp__fake__echo".to_owned(),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
        }],
        payload: serde_json::json!({
            "server": "fake",
            "arguments": {"type": "object", "top_level_keys": ["value"]}
        }),
        redacted: true,
    };
    let session_entry = SessionLogEntry::Control(ControlEntry::ToolEgress(Box::new(entry.clone())));

    let json = serde_json::to_string(&session_entry)?;
    let decoded: SessionLogEntry = serde_json::from_str(&json)?;

    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::ToolEgress(restored))
            if *restored == entry
    ));
    Ok(())
}

#[test]
fn mcp_elicitation_control_entry_roundtrips_without_content_values() -> Result<()> {
    let entry = McpElicitationEntry::new(
        "filesystem",
        "Need an access token for workspace path",
        &serde_json::json!({
            "type": "object",
            "properties": {
                "token": { "type": "string", "title": "Token" },
                "path": { "type": "string", "title": "Path" }
            },
            "required": ["token"]
        }),
        McpElicitationDecision::Accepted,
        Some(&serde_json::json!({
            "token": "secret-token-value",
            "path": "src/lib.rs"
        })),
    );
    let session_entry =
        SessionLogEntry::Control(ControlEntry::McpElicitation(Box::new(entry.clone())));

    let json = serde_json::to_string(&session_entry)?;
    let decoded: SessionLogEntry = serde_json::from_str(&json)?;

    assert!(!json.contains("secret-token-value"));
    assert!(!json.contains("src/lib.rs"));
    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::McpElicitation(restored))
            if *restored == entry
                && restored.content_redacted
                && restored.content_field_names == vec!["path".to_owned(), "token".to_owned()]
                && restored.required_field_names == vec!["token".to_owned()]
    ));
    Ok(())
}

#[test]
fn verification_control_entries_roundtrip_with_snake_case_payloads() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(
            sample_check_spec_recorded_entry(),
        )),
        SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(
            sample_verification_policy_changed_entry()?,
        )),
        SessionLogEntry::Control(ControlEntry::VerificationRecorded(
            sample_verification_recorded_entry(),
        )),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
            sample_readiness_evaluated_entry(),
        )),
        SessionLogEntry::Control(ControlEntry::ChildVerificationReceiptLinked(
            sample_child_verification_receipt_linked(),
        )),
        SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(
            sample_workspace_trust_decision_entry(),
        )),
    ];

    for entry in entries {
        let json = serde_json::to_string(&entry)?;
        let decoded: SessionLogEntry = serde_json::from_str(&json)?;

        match (entry, decoded) {
            (
                SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(expected)),
                SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(restored)),
            ) => assert_eq!(restored, expected),
            (
                SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(expected)),
                SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(restored)),
            ) => assert_eq!(restored, expected),
            (
                SessionLogEntry::Control(ControlEntry::VerificationRecorded(expected)),
                SessionLogEntry::Control(ControlEntry::VerificationRecorded(restored)),
            ) => assert_eq!(restored, expected),
            (
                SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(expected)),
                SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(restored)),
            ) => assert_eq!(restored, expected),
            (
                SessionLogEntry::Control(ControlEntry::ChildVerificationReceiptLinked(expected)),
                SessionLogEntry::Control(ControlEntry::ChildVerificationReceiptLinked(restored)),
            ) => assert_eq!(restored, expected),
            (
                SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(expected)),
                SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(restored)),
            ) => assert_eq!(restored, expected),
            (_, decoded) => panic!("unexpected decoded verification entry: {decoded:?}"),
        }
    }
    Ok(())
}

#[test]
fn append_session_entry_event_writes_verification_durable_event_types() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let cases = vec![
        (
            SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(
                sample_check_spec_recorded_entry(),
            )),
            DurableEventType::CheckSpecRecorded,
        ),
        (
            SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(
                sample_verification_policy_changed_entry()?,
            )),
            DurableEventType::VerificationPolicyChanged,
        ),
        (
            SessionLogEntry::Control(ControlEntry::VerificationRecorded(
                sample_verification_recorded_entry(),
            )),
            DurableEventType::VerificationRecorded,
        ),
        (
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
                sample_readiness_evaluated_entry(),
            )),
            DurableEventType::ReadinessEvaluated,
        ),
        (
            SessionLogEntry::Control(ControlEntry::ChildVerificationReceiptLinked(
                sample_child_verification_receipt_linked(),
            )),
            DurableEventType::ChildVerificationReceiptLinked,
        ),
        (
            SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(
                sample_workspace_trust_decision_entry(),
            )),
            DurableEventType::WorkspaceTrustDecision,
        ),
    ];

    for (entry, expected_event_type) in cases {
        let stored = store.append_session_entry_event(&entry)?;

        assert_eq!(stored.event_type, expected_event_type.as_str());
        assert_eq!(stored.event_class, EventClass::Critical);
    }
    Ok(())
}

#[test]
fn verification_state_projection_replays_control_entries() -> Result<()> {
    let check_spec_entry = sample_check_spec_recorded_entry();
    let policy_entry = sample_verification_policy_changed_entry()?;
    let check_run_entry = sample_verification_check_run_entry();
    let recorded_entry = sample_verification_recorded_entry();
    let readiness_entry = sample_readiness_evaluated_entry();
    let child_link = sample_child_verification_receipt_linked();
    let trust_entry = sample_workspace_trust_decision_entry();
    let scope = policy_entry.scope.clone();
    let check_run_id = check_run_entry.run_id.clone();
    let receipt_id = recorded_entry.receipt.receipt.receipt_id.clone();
    let workspace_id = trust_entry.workspace_id.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "unrelated".to_owned(),
            data: serde_json::json!({}),
        }),
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(check_spec_entry.clone())),
        SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(
            policy_entry.clone(),
        )),
        SessionLogEntry::Control(ControlEntry::VerificationCheckRun(check_run_entry.clone())),
        SessionLogEntry::Control(ControlEntry::VerificationRecorded(recorded_entry.clone())),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness_entry.clone())),
        SessionLogEntry::Control(ControlEntry::ChildVerificationReceiptLinked(
            child_link.clone(),
        )),
        SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(trust_entry.clone())),
    ];

    let projection = VerificationStateProjection::from_entries(&entries);

    assert_eq!(
        projection.check_spec(&check_spec_entry.scope, "cargo-test"),
        Some(&check_spec_entry)
    );
    assert_eq!(projection.latest_policy(&scope), Some(&policy_entry));
    assert_eq!(projection.check_run(&check_run_id), Some(&check_run_entry));
    assert_eq!(projection.receipt(&receipt_id), Some(&recorded_entry));
    assert_eq!(projection.latest_readiness(&scope), Some(&readiness_entry));
    assert_eq!(projection.child_receipt_links, vec![child_link]);
    assert_eq!(
        projection.workspace_trust.get(&workspace_id),
        Some(&trust_entry)
    );
    Ok(())
}

#[test]
fn session_exposes_verification_state_projection() -> Result<()> {
    let policy_entry = sample_verification_policy_changed_entry()?;
    let scope = policy_entry.scope.clone();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    assert!(
        session
            .try_verification_state_projection_from_durable()?
            .is_none()
    );

    session.append_control(ControlEntry::VerificationPolicyChanged(
        policy_entry.clone(),
    ))?;

    let projection = session.verification_state_projection();

    assert_eq!(projection.latest_policy(&scope), Some(&policy_entry));
    Ok(())
}

#[test]
fn session_exposes_optional_durable_task_state_projection() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    assert!(session.try_task_state_projection_from_durable()?.is_none());

    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: test_task_id(),
        parent_session_ref: test_session_ref(),
        objective: "implement task replay".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }))?;

    let projection = session.task_state_projection();

    assert_eq!(projection.latest_task_id.as_ref(), Some(&test_task_id()));
    assert!(projection.tasks.contains_key(&test_task_id()));
    Ok(())
}

#[test]
fn optional_durable_projections_return_none_for_in_memory_sessions() -> Result<()> {
    let session = Session::new("deepseek", "deepseek-v4-flash");

    assert!(
        session
            .try_plan_approval_projection_from_durable()?
            .is_none()
    );
    assert!(session.try_task_state_projection_from_durable()?.is_none());
    assert!(
        session
            .try_agent_thread_state_projection_from_durable()?
            .is_none()
    );
    assert!(session.try_agent_graph_projection_from_durable()?.is_none());
    assert!(
        session
            .try_session_list_projection_from_durable()?
            .is_none()
    );
    assert!(
        session
            .try_dispatch_trace_projection_from_durable()?
            .is_none()
    );
    assert!(
        session
            .try_agent_profile_trust_projection_from_durable()?
            .is_none()
    );
    assert!(
        session
            .try_agent_profile_policy_projection_from_durable()?
            .is_none()
    );
    assert!(session.try_skill_state_projection_from_durable()?.is_none());
    assert!(
        session
            .try_plugin_state_projection_from_durable()?
            .is_none()
    );
    assert!(session.try_changeset_projection_from_durable()?.is_none());
    assert!(
        session
            .try_verification_state_projection_from_durable()?
            .is_none()
    );
    assert!(
        session
            .try_terminal_task_projection_from_durable()?
            .is_none()
    );
    assert!(
        session
            .try_conversation_queue_projection_from_durable()?
            .is_none()
    );
    assert!(
        session
            .try_agent_result_continuation_projection_from_durable()?
            .is_none()
    );
    assert!(session.try_usage_stats_from_durable()?.is_none());
    Ok(())
}

#[test]
fn task_state_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy_run = SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: test_task_id(),
        parent_session_ref: test_session_ref(),
        objective: "ship durable projection".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }));
    fs::write(&path, format!("{}\n", serde_json::to_string(&legacy_run)?))?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::TaskPlan(
        TaskPlanEntry {
            task_id: test_task_id(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        },
    )))?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::TaskStep(
        TaskStepEntry {
            task_id: test_task_id(),
            plan_version: 1,
            step_id: test_step_id(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("implement".to_owned()),
            summary: Some("done".to_owned()),
            reason: None,
        },
    )))?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::TaskRun(
        TaskRunEntry {
            task_id: test_task_id(),
            parent_session_ref: test_session_ref(),
            objective: "ship durable projection".to_owned(),
            status: TaskRunStatus::Completed,
            reason: Some("finished".to_owned()),
        },
    )))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_task_state_projection_from_durable()?
        .expect("durable session should replay task projection");
    let task = projection
        .tasks
        .get(&test_task_id())
        .expect("task should replay from mixed stream");

    assert_eq!(projection.latest_task_id.as_ref(), Some(&test_task_id()));
    assert_eq!(task.status, TaskRunStatus::Completed);
    assert_eq!(task.latest_plan_version, Some(1));
    assert_eq!(
        task.steps
            .get(&(1, test_step_id()))
            .map(|step| (&step.status, step.summary.as_deref())),
        Some((&TaskStepStatus::Completed, Some("done")))
    );
    Ok(())
}

#[test]
fn task_projection_record_helper_fails_closed_on_sequence_gap() -> Result<()> {
    let event = StoredEvent::new(
        DurableEventType::TaskStatusChanged,
        EventClass::Critical,
        "event-2".to_owned(),
        "session-gap".to_owned(),
        2,
        serde_json::json!({}),
    )?;
    let record = SessionStreamRecord::Stored(event);
    let mut projection = TaskStateProjection::default();
    let mut cursor = Some(ProjectionCursor {
        session_id: "session-gap".to_owned(),
        projection_schema_version: super::TASK_STATE_PROJECTION_SCHEMA_VERSION,
        last_applied_stream_sequence: 0,
        last_applied_event_id: "event-0".to_owned(),
        last_applied_record_checksum: "sha256:0".to_owned(),
    });

    let error = super::apply_task_projection_record(&mut projection, &mut cursor, &record)
        .expect_err("projection should fail closed on sequence gaps");

    assert!(error.to_string().contains("projection sequence gap"));
    assert!(projection.tasks.is_empty());
    Ok(())
}

#[test]
fn task_projection_record_helper_fails_closed_on_unknown_critical_event() -> Result<()> {
    let event = StoredEvent::new_raw(
        "future_task_event",
        EventClass::Critical,
        "event-future".to_owned(),
        "session-task".to_owned(),
        1,
        serde_json::json!({"value": "must not be ignored"}),
    )?;
    let record = SessionStreamRecord::Stored(event);
    let mut projection = TaskStateProjection::default();
    let mut cursor = None;

    let error = super::apply_task_projection_record(&mut projection, &mut cursor, &record)
        .expect_err("unknown critical event should fail closed");

    assert!(
        error
            .to_string()
            .contains("unknown critical event future_task_event")
    );
    assert!(projection.tasks.is_empty());
    assert!(cursor.is_none());
    Ok(())
}

#[test]
fn typed_domain_event_record_decodes_projection_cursor() -> Result<()> {
    let event = StoredEvent::new(
        DurableEventType::MutationCommitted,
        EventClass::Critical,
        "event-mutation-commit".to_owned(),
        "session-typed".to_owned(),
        7,
        serde_json::json!({
            "operation_id": "op-1",
            "workspace_id": "workspace-1",
            "observed_after_hash": "sha256:after",
            "workspace_revision": 3,
            "workspace_snapshot_id": "snapshot-3",
            "committed_subject": {
                "file": {
                    "path": "README.md",
                    "file_type": "file"
                }
            }
        }),
    )?;
    let record = SessionStreamRecord::Stored(event);

    let typed = record
        .typed_domain_event_record()?
        .expect("typed event should be exposed");

    assert!(matches!(
        typed.event,
        TypedDomainEvent::MutationCommitted(ref payload)
            if payload.operation_id == "op-1" && payload.workspace_revision == 3
    ));
    assert_eq!(typed.cursor.session_id, "session-typed");
    assert_eq!(typed.cursor.last_applied_stream_sequence, 7);
    assert_eq!(typed.cursor.last_applied_event_id, "event-mutation-commit");
    Ok(())
}

#[test]
fn typed_domain_event_record_ignores_legacy_and_unknown_noncritical() -> Result<()> {
    let legacy = SessionStreamRecord::Legacy {
        event: LegacyEvent {
            event_id: "event-legacy".to_owned(),
            session_id: "session-typed".to_owned(),
            stream_sequence: 1,
            raw_line_hash: "sha256:legacy".to_owned(),
            payload: serde_json::json!({"legacy": true}),
        },
        entry: Box::new(SessionLogEntry::User(ModelMessage::user("legacy"))),
    };
    assert!(legacy.typed_domain_event_record()?.is_none());

    let future = StoredEvent::new_raw(
        "future_noncritical_event",
        EventClass::NonCritical,
        "event-future".to_owned(),
        "session-typed".to_owned(),
        2,
        serde_json::json!({"value": "ignore"}),
    )?;
    let record = SessionStreamRecord::Stored(future);
    assert!(record.typed_domain_event_record()?.is_none());
    Ok(())
}

#[test]
fn session_exposes_optional_durable_agent_thread_state_projection() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    assert!(
        session
            .try_agent_thread_state_projection_from_durable()?
            .is_none()
    );

    session.append_control(ControlEntry::AgentThreadStarted(
        test_agent_thread_started_entry(),
    ))?;

    let projection = session.agent_thread_state_projection();

    assert_eq!(
        projection.latest_thread_id.as_ref(),
        Some(&test_agent_thread_id())
    );
    assert!(projection.threads.contains_key(&test_agent_thread_id()));
    Ok(())
}

#[test]
fn agent_thread_state_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let started = test_agent_thread_started_entry();
    let legacy_started =
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(started.clone()));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy_started)?),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
            thread_id: test_agent_thread_id(),
            status: AgentThreadStatus::Completed,
            reason: Some("finished".to_owned()),
            updated_at_ms: Some(2),
        }),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_agent_thread_state_projection_from_durable()?
        .expect("durable session should replay agent thread projection");
    let thread = projection
        .threads
        .get(&test_agent_thread_id())
        .expect("agent thread should replay from mixed stream");

    assert_eq!(
        projection.latest_thread_id.as_ref(),
        Some(&test_agent_thread_id())
    );
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(thread.reason.as_deref(), Some("finished"));
    assert_eq!(
        thread.thread_session_ref.as_ref(),
        Some(&started.thread_session_ref)
    );
    assert_eq!(thread.profile_id.as_ref(), Some(&started.profile_id));
    Ok(())
}

#[test]
fn session_list_projection_replays_from_session_durable_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let identity = SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
    });
    fs::write(&path, format!("{}\n", serde_json::to_string(&identity)?))?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::User(ModelMessage::user(
        "Inspect durable replay adoption",
    )))?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        UsageStats {
            prompt_tokens: 13,
            completion_tokens: 5,
            cache_hit_tokens: 7,
            cache_miss_tokens: 6,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        },
    )))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_session_list_projection_from_durable()?
        .expect("durable session should replay session-list projection");
    let entry = projection
        .latest_session()
        .expect("session-list projection should contain this session");

    assert_eq!(entry.provider_name.as_deref(), Some("deepseek"));
    assert_eq!(entry.model_name.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(
        entry.title.as_deref(),
        Some("Inspect durable replay adoption")
    );
    assert_eq!(
        entry.latest_usage.as_ref().map(|usage| usage.prompt_tokens),
        Some(13)
    );
    Ok(())
}

#[test]
fn dispatch_trace_projection_replays_from_session_durable_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::ToolEgress(
        Box::new(ToolEgressEntry {
            call_id: "call-egress".to_owned(),
            tool_name: "webfetch".to_owned(),
            destination: "https://example.test".to_owned(),
            operation: "request".to_owned(),
            subjects: vec![ToolSubjectAudit {
                kind: ToolSubjectKind::NetworkEndpoint,
                original: "https://example.test".to_owned(),
                normalized: "https://example.test".to_owned(),
                canonical_path: None,
                scope: ToolSubjectScope::External,
            }],
            payload: serde_json::json!({"secret": "must-not-project"}),
            redacted: true,
        }),
    )))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ReadinessEvaluated(sample_readiness_evaluated_entry()),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_dispatch_trace_projection_from_durable()?
        .expect("durable session should replay dispatch-trace projection");
    let trace = projection
        .trace("tool:call-egress")
        .expect("tool egress should materialize as dispatch trace");
    let encoded = serde_json::to_string(&projection)?;

    assert_eq!(projection.summary.egress_events, 1);
    assert_eq!(projection.summary.redacted_egress_events, 1);
    assert_eq!(trace.tool_name.as_deref(), Some("webfetch"));
    assert_eq!(
        trace.egress_destinations,
        vec!["https://example.test".to_owned()]
    );
    assert_eq!(
        projection
            .latest_readiness
            .as_ref()
            .map(|readiness| readiness.visible_state),
        Some(VisibleCompletionState::CompletedUnverified)
    );
    assert!(!encoded.contains("must-not-project"));
    Ok(())
}

#[test]
fn new_projection_adapters_fail_closed_on_corrupt_stream_sequence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let event = StoredEvent::new(
        DurableEventType::UserMessageRecorded,
        EventClass::Critical,
        "event-gap".to_owned(),
        "session-gap".to_owned(),
        2,
        serde_json::json!({
            "session_log_entry": SessionLogEntry::User(ModelMessage::user("gap"))
        }),
    )?;
    fs::write(&path, event.to_json_line()?)?;
    let store = JsonlSessionStore::new(&path)?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let session_list_error = session
        .try_session_list_projection_from_durable()
        .expect_err("session-list adapter should fail closed on stream gap");
    let dispatch_trace_error = session
        .try_dispatch_trace_projection_from_durable()
        .expect_err("dispatch-trace adapter should fail closed on stream gap");

    assert!(
        session_list_error
            .to_string()
            .contains("stream_sequence does not match expected sequence")
    );
    assert!(
        dispatch_trace_error
            .to_string()
            .contains("stream_sequence does not match expected sequence")
    );
    Ok(())
}

#[test]
fn agent_thread_projection_record_helper_fails_closed_on_sequence_gap() -> Result<()> {
    let event = StoredEvent::new(
        DurableEventType::SessionEntryRecorded,
        EventClass::NonCritical,
        "event-2".to_owned(),
        "session-gap".to_owned(),
        2,
        serde_json::json!({}),
    )?;
    let record = SessionStreamRecord::Stored(event);
    let mut projection = AgentThreadStateProjection::default();
    let mut cursor = Some(ProjectionCursor {
        session_id: "session-gap".to_owned(),
        projection_schema_version: super::AGENT_THREAD_STATE_PROJECTION_SCHEMA_VERSION,
        last_applied_stream_sequence: 0,
        last_applied_event_id: "event-0".to_owned(),
        last_applied_record_checksum: "sha256:0".to_owned(),
    });

    let error = super::apply_agent_thread_projection_record(&mut projection, &mut cursor, &record)
        .expect_err("projection should fail closed on sequence gaps");

    assert!(error.to_string().contains("projection sequence gap"));
    assert!(projection.threads.is_empty());
    Ok(())
}

#[test]
fn agent_thread_projection_record_helper_fails_closed_on_unknown_critical_event() -> Result<()> {
    let event = StoredEvent::new_raw(
        "future_agent_thread_event",
        EventClass::Critical,
        "event-future-agent".to_owned(),
        "session-agent-thread".to_owned(),
        1,
        serde_json::json!({"value": "must not be ignored"}),
    )?;
    let record = SessionStreamRecord::Stored(event);
    let mut projection = AgentThreadStateProjection::default();
    let mut cursor = None;

    let error = super::apply_agent_thread_projection_record(&mut projection, &mut cursor, &record)
        .expect_err("unknown critical event should fail closed");

    assert!(
        error
            .to_string()
            .contains("unknown critical event future_agent_thread_event")
    );
    assert!(projection.threads.is_empty());
    assert!(cursor.is_none());
    Ok(())
}

#[test]
fn agent_profile_projections_replay_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let snapshot = AgentProfileSnapshot {
        snapshot_id: test_agent_profile_snapshot_id(),
        profile_id: test_agent_profile_id(),
        source: AgentProfileSource::Workspace,
        source_hash: "sha256:source".to_owned(),
        profile_hash: "sha256:profile".to_owned(),
        resolved_tool_scope_hash: "sha256:tools".to_owned(),
        resolved_permission_policy_hash: "sha256:permissions".to_owned(),
        resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
        resolved_skill_hashes: vec!["sha256:skill".to_owned()],
        trust_state: AgentTrustState::NeedsReview,
    };
    let legacy_trust = AgentProfileTrustEntry {
        profile_id: snapshot.profile_id.clone(),
        source: snapshot.source.clone(),
        source_hash: snapshot.source_hash.clone(),
        profile_hash: snapshot.profile_hash.clone(),
        decision: AgentTrustState::Disabled,
        reviewed_at_ms: 10,
    };
    let legacy_policy = AgentProfilePolicyEntry {
        profile_id: snapshot.profile_id.clone(),
        source: snapshot.source.clone(),
        source_hash: snapshot.source_hash.clone(),
        profile_hash: snapshot.profile_hash.clone(),
        enabled: Some(false),
        user_invocable: Some(false),
        model_invocable: Some(false),
        reviewed_at_ms: 11,
    };
    fs::write(
        &path,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&SessionLogEntry::Control(
                ControlEntry::AgentProfileTrustDecision(legacy_trust)
            ))?,
            serde_json::to_string(&SessionLogEntry::Control(
                ControlEntry::AgentProfilePolicyDecision(legacy_policy)
            ))?
        ),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    let trusted = AgentProfileTrustEntry {
        profile_id: snapshot.profile_id.clone(),
        source: snapshot.source.clone(),
        source_hash: snapshot.source_hash.clone(),
        profile_hash: snapshot.profile_hash.clone(),
        decision: AgentTrustState::Trusted,
        reviewed_at_ms: 20,
    };
    let enabled = AgentProfilePolicyEntry {
        profile_id: snapshot.profile_id.clone(),
        source: snapshot.source.clone(),
        source_hash: snapshot.source_hash.clone(),
        profile_hash: snapshot.profile_hash.clone(),
        enabled: Some(true),
        user_invocable: Some(true),
        model_invocable: Some(true),
        reviewed_at_ms: 21,
    };
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentProfileTrustDecision(trusted),
    ))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentProfilePolicyDecision(enabled.clone()),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let trust_projection = session
        .try_agent_profile_trust_projection_from_durable()?
        .expect("durable session should replay profile trust");
    let policy_projection = session
        .try_agent_profile_policy_projection_from_durable()?
        .expect("durable session should replay profile policy");

    assert_eq!(
        trust_projection.decision_for_snapshot(&snapshot),
        Some(AgentTrustState::Trusted)
    );
    assert_eq!(
        trust_projection.trust_replay_order,
        vec![snapshot.profile_id.clone(), snapshot.profile_id.clone()]
    );
    assert_eq!(
        policy_projection.policy_for_snapshot(&snapshot),
        Some(&enabled)
    );
    assert_eq!(
        policy_projection.policy_replay_order,
        vec![snapshot.profile_id.clone(), snapshot.profile_id]
    );
    Ok(())
}

#[test]
fn agent_result_continuation_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let thread = test_agent_thread_id();
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::Control(
                ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
                    thread_id: thread.clone(),
                    status: AgentResultContinuationStatus::Pending,
                    reason: Some("waiting for child".to_owned()),
                    updated_at_ms: Some(10),
                })
            ))?
        ),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
            thread_id: thread.clone(),
            status: AgentResultContinuationStatus::Completed,
            reason: Some("parent consumed child result".to_owned()),
            updated_at_ms: Some(20),
        }),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_agent_result_continuation_projection_from_durable()?
        .expect("durable session should replay continuation state");

    assert_eq!(
        projection.statuses.get(&thread),
        Some(&AgentResultContinuationStatus::Completed)
    );
    assert!(projection.pending_thread_ids.is_empty());
    Ok(())
}

#[test]
fn conversation_queue_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let queue_id = ConversationInputQueueId::new("queue-1")?;
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::Control(
                ControlEntry::ConversationInputQueued(ConversationInputQueuedEntry {
                    queue_id: queue_id.clone(),
                    target: ConversationInputTarget::MainThread,
                    kind: ConversationInputKind::Chat,
                    prompt_hash: "sha256:prompt".to_owned(),
                    prompt: "hello".to_owned(),
                    reasoning_effort: None,
                    created_at_ms: Some(10),
                })
            ))?
        ),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Pause,
            reason: Some("manual pause".to_owned()),
            updated_at_ms: Some(20),
        }),
    ))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id: queue_id.clone(),
            status: ConversationInputStatus::Dispatching,
            reason: Some("sending".to_owned()),
            updated_at_ms: Some(30),
        }),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_conversation_queue_projection_from_durable()?
        .expect("durable session should replay conversation queue");

    assert!(projection.paused);
    assert_eq!(projection.next_dispatchable, None);
    assert_eq!(projection.items.len(), 1);
    assert_eq!(projection.items[0].queued.queue_id, queue_id);
    assert_eq!(
        projection.items[0].status,
        ConversationInputStatus::Dispatching
    );
    assert_eq!(projection.items[0].reason.as_deref(), Some("sending"));
    Ok(())
}

#[test]
fn verification_state_projection_replays_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let check_spec_entry = sample_check_spec_recorded_entry();
    let policy_entry = sample_verification_policy_changed_entry()?;
    let check_run_entry = sample_verification_check_run_entry();
    let recorded_entry = sample_verification_recorded_entry();
    let readiness_entry = sample_readiness_evaluated_entry();
    let scope = policy_entry.scope.clone();
    let check_run_id = check_run_entry.run_id.clone();
    let receipt_id = recorded_entry.receipt.receipt.receipt_id.clone();
    for entry in [
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(check_spec_entry.clone())),
        SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(
            policy_entry.clone(),
        )),
        SessionLogEntry::Control(ControlEntry::VerificationCheckRun(check_run_entry.clone())),
        SessionLogEntry::Control(ControlEntry::VerificationRecorded(recorded_entry.clone())),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness_entry.clone())),
    ] {
        store.append_session_entry_event(&entry)?;
    }
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_verification_state_projection_from_durable()?
        .expect("durable session should replay verification projection");

    assert_eq!(
        projection.check_spec(&check_spec_entry.scope, "cargo-test"),
        Some(&check_spec_entry)
    );
    assert_eq!(projection.latest_policy(&scope), Some(&policy_entry));
    assert_eq!(projection.check_run(&check_run_id), Some(&check_run_entry));
    assert_eq!(projection.receipt(&receipt_id), Some(&recorded_entry));
    assert_eq!(projection.latest_readiness(&scope), Some(&readiness_entry));
    Ok(())
}

#[test]
fn verification_projection_record_helper_ignores_idempotent_replay() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let policy_entry = sample_verification_policy_changed_entry()?;
    let scope = policy_entry.scope.clone();
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::VerificationPolicyChanged(policy_entry.clone()),
    ))?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let record = records
        .first()
        .expect("one durable record should be present");
    let mut projection = VerificationStateProjection::default();
    let mut cursor = None;

    super::apply_verification_projection_record(&mut projection, &mut cursor, record)?;
    super::apply_verification_projection_record(&mut projection, &mut cursor, record)?;

    assert_eq!(projection.latest_policy(&scope), Some(&policy_entry));
    assert!(cursor.is_some());
    assert_eq!(projection.policies.len(), 1);
    Ok(())
}

#[test]
fn verification_projection_record_helper_fails_closed_on_sequence_gap() -> Result<()> {
    let event = StoredEvent::new(
        DurableEventType::VerificationPolicyChanged,
        EventClass::Critical,
        "event-2".to_owned(),
        "session-gap".to_owned(),
        2,
        serde_json::json!({}),
    )?;
    let record = SessionStreamRecord::Stored(event);
    let mut projection = VerificationStateProjection::default();
    let mut cursor = Some(ProjectionCursor {
        session_id: "session-gap".to_owned(),
        projection_schema_version: super::VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION,
        last_applied_stream_sequence: 0,
        last_applied_event_id: "event-0".to_owned(),
        last_applied_record_checksum: "sha256:0".to_owned(),
    });

    let error = super::apply_verification_projection_record(&mut projection, &mut cursor, &record)
        .expect_err("projection should fail closed on sequence gaps");

    assert!(error.to_string().contains("projection sequence gap"));
    assert!(projection.policies.is_empty());
    Ok(())
}

#[test]
fn session_changeset_projection_replays_control_entries() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let id = ChangeSetId::new("change-1")?;
    session.append_control(ControlEntry::ChangeSetProposed(ChangeSet {
        id: id.clone(),
        title: "Update README".to_owned(),
        summary: "Update project overview".to_owned(),
        risk: ChangeSetRisk::Low,
        files: Vec::new(),
        validations: Vec::new(),
    }))?;
    session.append_control(ControlEntry::ChangeSetApplied(ChangeSetResult {
        id: id.clone(),
        status: ChangeSetResultStatus::Applied,
        file_results: Vec::new(),
        message: None,
    }))?;

    let projection = session.changeset_projection();
    let latest = projection.latest().expect("latest changeset");

    assert_eq!(projection.latest_change_set_id.as_ref(), Some(&id));
    assert!(latest.proposal.is_some());
    assert!(matches!(
        latest.result.as_ref(),
        Some(result) if result.status == ChangeSetResultStatus::Applied
    ));
    Ok(())
}

#[test]
fn changeset_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let id = ChangeSetId::new("change-1")?;
    let legacy_proposal = SessionLogEntry::Control(ControlEntry::ChangeSetProposed(ChangeSet {
        id: id.clone(),
        title: "Update README".to_owned(),
        summary: "Update project overview".to_owned(),
        risk: ChangeSetRisk::Low,
        files: Vec::new(),
        validations: Vec::new(),
    }));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy_proposal)?),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::ChangeSetApplied(
        ChangeSetResult {
            id: id.clone(),
            status: ChangeSetResultStatus::Applied,
            file_results: Vec::new(),
            message: Some("applied".to_owned()),
        },
    )))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_changeset_projection_from_durable()?
        .expect("durable session should replay changeset projection");
    let latest = projection.latest().expect("latest changeset");

    assert_eq!(projection.latest_change_set_id.as_ref(), Some(&id));
    assert!(latest.proposal.is_some());
    assert!(matches!(
        latest.result.as_ref(),
        Some(result)
            if result.status == ChangeSetResultStatus::Applied
                && result.message.as_deref() == Some("applied")
    ));
    Ok(())
}

#[test]
fn plan_approval_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = PlanApprovedEntry {
        plan_version: 1,
        plan_hash: "sha256:first".to_owned(),
        approved_at_ms: 10,
        permission: PlanApprovalPermission::Ask,
        scope: PlanApprovalScope {
            summary: "first plan".to_owned(),
            workspace_paths: Vec::new(),
        },
        expires: PlanApprovalExpiry::NextUserPrompt,
        clear_planning_context: false,
    };
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::Control(ControlEntry::PlanApproved(first)))?
        ),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    let second = PlanApprovedEntry {
        plan_version: 2,
        plan_hash: "sha256:second".to_owned(),
        approved_at_ms: 20,
        permission: PlanApprovalPermission::WorkspaceEdits,
        scope: PlanApprovalScope {
            summary: "second plan".to_owned(),
            workspace_paths: vec!["README.md".to_owned()],
        },
        expires: PlanApprovalExpiry::Session,
        clear_planning_context: true,
    };
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::PlanApproved(
        second.clone(),
    )))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_plan_approval_projection_from_durable()?
        .expect("durable session should replay plan approvals");

    assert_eq!(projection.approvals.len(), 2);
    assert_eq!(projection.latest_approval, Some(second.clone()));
    assert_eq!(
        projection.latest_by_hash.get("sha256:second"),
        Some(&second)
    );
    Ok(())
}

#[test]
fn session_terminal_task_projection_replays_control_entries() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let id = TerminalTaskId::new("terminal-1")?;
    session.append_control(ControlEntry::TerminalTask(TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id: id.clone(),
            command: "cargo test".to_owned(),
            cwd: ".".into(),
            shell: "zsh".to_owned(),
            log_path: ".sigil/terminal/terminal-1/output.log".into(),
            created_at_ms: 100,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        status: TerminalTaskStatus::Running,
        output_preview: Some("running tests".to_owned()),
        output_hash: Some("sha256:abc".to_owned()),
        output_truncated: false,
        cleanup: None,
        updated_at_ms: 120,
    }))?;

    let projection = session.terminal_task_projection();
    let latest = projection.latest().expect("latest terminal task");

    assert_eq!(projection.latest_task_id.as_ref(), Some(&id));
    assert_eq!(projection.active_task_ids, vec![id]);
    assert!(matches!(latest.status, TerminalTaskStatus::Running));
    Ok(())
}

#[test]
fn terminal_task_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let id = TerminalTaskId::new("terminal-1")?;
    let legacy_running = SessionLogEntry::Control(ControlEntry::TerminalTask(TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id: id.clone(),
            command: "cargo test".to_owned(),
            cwd: ".".into(),
            shell: "zsh".to_owned(),
            log_path: ".sigil/terminal/terminal-1/output.log".into(),
            created_at_ms: 100,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        status: TerminalTaskStatus::Running,
        output_preview: Some("running tests".to_owned()),
        output_hash: Some("sha256:abc".to_owned()),
        output_truncated: false,
        cleanup: None,
        updated_at_ms: 120,
    }));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy_running)?),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::TerminalTask(
        TerminalTaskEntry {
            handle: TerminalTaskHandle {
                task_id: id.clone(),
                command: "cargo test".to_owned(),
                cwd: ".".into(),
                shell: "zsh".to_owned(),
                log_path: ".sigil/terminal/terminal-1/output.log".into(),
                created_at_ms: 100,
                execution_backend: None,
                execution_backend_capabilities: None,
                enforcement_backend: None,
                enforcement_backend_capabilities: None,
                sandbox_profile: None,
            },
            status: TerminalTaskStatus::Exited { exit_code: Some(0) },
            output_preview: Some("ok".to_owned()),
            output_hash: Some("sha256:def".to_owned()),
            output_truncated: false,
            cleanup: None,
            updated_at_ms: 180,
        },
    )))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_terminal_task_projection_from_durable()?
        .expect("durable session should replay terminal projection");
    let latest = projection.latest().expect("latest terminal task");

    assert_eq!(projection.latest_task_id.as_ref(), Some(&id));
    assert!(projection.active_task_ids.is_empty());
    assert!(matches!(
        latest.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    assert_eq!(latest.output_preview.as_deref(), Some("ok"));
    Ok(())
}

#[test]
fn session_skill_state_projection_replays_control_entries() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let snapshot = SkillIndexSnapshot::new(vec![SkillDescriptor {
        id: "repo-review".to_owned(),
        name: "Repo Review".to_owned(),
        description: "Review repository changes".to_owned(),
        when_to_use: Some("Use for repository code review.".to_owned()),
        root: ".sigil/skills/repo-review".into(),
        entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
        source: SkillSource::Workspace,
        sha256: "hash".to_owned(),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::Inline,
        agent: None,
        argument_hint: None,
        allowed_tools: Default::default(),
        disallowed_tools: Default::default(),
        path_patterns: Vec::new(),
    }])?;
    session.append_control(ControlEntry::SkillIndexCaptured(snapshot.clone()))?;
    session.append_control(ControlEntry::SkillLoaded(SkillLoadEntry {
        skill_id: "repo-review".to_owned(),
        sha256: "hash".to_owned(),
        source: SkillSource::Workspace,
        entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
        run_id: Some("run-1".to_owned()),
        call_id: Some("call-1".to_owned()),
        byte_count: 128,
        line_count: 7,
        loaded_at_ms: 42,
    }))?;

    let projection = session.skill_state_projection();
    let latest_loaded = projection.latest_loaded().expect("latest loaded skill");

    assert_eq!(projection.latest_index, Some(snapshot));
    assert_eq!(
        projection.latest_loaded_skill_id.as_deref(),
        Some("repo-review")
    );
    assert_eq!(latest_loaded.entry.byte_count, 128);
    Ok(())
}

#[test]
fn skill_state_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let snapshot = SkillIndexSnapshot::new(vec![SkillDescriptor {
        id: "repo-review".to_owned(),
        name: "Repo Review".to_owned(),
        description: "Review repository changes".to_owned(),
        when_to_use: Some("Use for repository code review.".to_owned()),
        root: ".sigil/skills/repo-review".into(),
        entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
        source: SkillSource::Workspace,
        sha256: "hash".to_owned(),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::Inline,
        agent: None,
        argument_hint: None,
        allowed_tools: Default::default(),
        disallowed_tools: Default::default(),
        path_patterns: Vec::new(),
    }])?;
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(
                snapshot.clone()
            )))?
        ),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::SkillLoaded(
        SkillLoadEntry {
            skill_id: "repo-review".to_owned(),
            sha256: "hash".to_owned(),
            source: SkillSource::Workspace,
            entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
            run_id: Some("run-1".to_owned()),
            call_id: Some("call-1".to_owned()),
            byte_count: 128,
            line_count: 7,
            loaded_at_ms: 42,
        },
    )))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_skill_state_projection_from_durable()?
        .expect("durable session should replay skills");
    let latest_loaded = projection.latest_loaded().expect("latest loaded skill");

    assert_eq!(projection.latest_index, Some(snapshot));
    assert_eq!(
        projection.latest_loaded_skill_id.as_deref(),
        Some("repo-review")
    );
    assert_eq!(latest_loaded.entry.byte_count, 128);
    Ok(())
}

#[test]
fn session_plugin_state_projection_replays_control_entries() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let snapshot = PluginManifestSnapshot {
        plugin_id: "repo-review".to_owned(),
        name: "Repository Review".to_owned(),
        version: "0.1.0".to_owned(),
        description: None,
        manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
        manifest_hash: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            .to_owned(),
        capabilities: vec![PluginCapability::Skill {
            path: "skills/review/SKILL.md".into(),
        }],
        trust: PluginTrustDecision::NeedsReview,
    };
    let trust = PluginTrustEntry::for_snapshot(&snapshot, PluginTrustDecision::Trusted, 42)?;
    session.append_control(ControlEntry::PluginManifestCaptured(snapshot))?;
    session.append_control(ControlEntry::PluginTrustDecision(trust.clone()))?;

    let projection = session.plugin_state_projection();
    let latest_manifest = projection
        .latest_manifest()
        .expect("latest plugin manifest");
    let latest_trust = projection.latest_trust().expect("latest plugin trust");

    assert_eq!(latest_manifest.plugin_id, "repo-review");
    assert_eq!(latest_manifest.trust, PluginTrustDecision::Trusted);
    assert_eq!(latest_trust, &trust);
    Ok(())
}

#[test]
fn plugin_state_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let snapshot = PluginManifestSnapshot {
        plugin_id: "repo-review".to_owned(),
        name: "Repository Review".to_owned(),
        version: "0.1.0".to_owned(),
        description: None,
        manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
        manifest_hash: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            .to_owned(),
        capabilities: vec![PluginCapability::Skill {
            path: "skills/review/SKILL.md".into(),
        }],
        trust: PluginTrustDecision::NeedsReview,
    };
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::Control(
                ControlEntry::PluginManifestCaptured(snapshot.clone())
            ))?
        ),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    let trust = PluginTrustEntry::for_snapshot(&snapshot, PluginTrustDecision::Trusted, 42)?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::PluginTrustDecision(trust.clone()),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_plugin_state_projection_from_durable()?
        .expect("durable session should replay plugins");
    let latest_manifest = projection
        .latest_manifest()
        .expect("latest plugin manifest");
    let latest_trust = projection.latest_trust().expect("latest plugin trust");

    assert_eq!(latest_manifest.plugin_id, "repo-review");
    assert_eq!(latest_manifest.trust, PluginTrustDecision::Trusted);
    assert_eq!(latest_trust, &trust);
    Ok(())
}

#[test]
fn build_request_persists_prefix_snapshot_in_memory_and_store() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    fs::write(temp.path().join("AGENTS.md"), "repo rules\n")?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("hello"))?;

    let request = session.build_request(
        temp.path(),
        &MemoryConfig { enabled: true },
        Vec::new(),
        None,
        None,
        None,
    )?;

    assert_eq!(request.provider_name, "deepseek");
    assert!(
        request
            .messages
            .iter()
            .any(|message| matches!(message.role, crate::MessageRole::System))
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(_))
        )
    }));

    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
        )
    }));
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(_))
        )
    }));
    Ok(())
}

#[test]
fn build_request_injects_context_v0_dynamic_suffix_from_session_archive() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("Earlier parser investigation"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("The note parser rejected note.txt input during validation".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user(
        "What did we learn about parser validation?",
    ))?;

    let first = session.build_request(
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
    )?;
    let context_messages = request_context_v0_messages(&first);
    assert_eq!(context_messages.len(), 1);
    let context = context_messages[0];
    assert!(matches!(context.role, crate::MessageRole::System));
    let context_text = context.content.as_deref().expect("context content");
    assert!(context_text.contains("sigil_context_v0"));
    assert!(context_text.contains("session-archive:"));
    assert!(context_text.contains("parser rejected"));
    assert!(context_text.contains("retrieval_hit"));

    let context_index = first
        .messages
        .iter()
        .position(|message| message.id == context.id)
        .expect("context message position");
    let first_conversation_index = first
        .messages
        .iter()
        .position(|message| message.content.as_deref() == Some("Earlier parser investigation"))
        .expect("first projected conversation message");
    assert!(context_index < first_conversation_index);

    let second = session.build_request(
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
    )?;
    let second_context = request_context_v0_messages(&second);
    assert_eq!(second_context.len(), 1);
    assert_eq!(context.id, second_context[0].id);
    assert_eq!(context.content, second_context[0].content);
    Ok(())
}

#[test]
fn build_request_injects_context_v0_from_latest_task_memory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
        summary: "typed task memory summary".to_owned(),
        compacted_message_count: 0,
        retained_tail_message_count: 0,
        task_memory: Some(TaskMemoryV1 {
            memory_id: "runtime-memory".to_owned(),
            branch_id: None,
            valid_for_snapshot: "snapshot-runtime".to_owned(),
            supersedes: None,
            source_event_ids: vec!["event-objective".to_owned()],
            objective: "Keep context provenance inspectable".to_owned(),
            constraints: Vec::new(),
            decisions: Vec::new(),
            files_changed: Vec::new(),
            commands_run: Vec::new(),
            verification_results: Vec::new(),
            failed_attempts: Vec::new(),
            risks: Vec::new(),
            unresolved_issues: Vec::new(),
        }),
    }))?;
    session.append_user_message(ModelMessage::user("What context should I keep in mind?"))?;

    let request = session.build_request(
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
    )?;

    let context_messages = request_context_v0_messages(&request);
    assert_eq!(context_messages.len(), 1);
    let context_text = context_messages[0]
        .content
        .as_deref()
        .expect("context content");
    assert!(context_text.contains("task-memory:runtime-memory:objective"));
    assert!(context_text.contains("Keep context provenance inspectable"));
    assert!(context_text.contains("event-objective"));
    Ok(())
}

#[test]
fn build_request_injects_context_v0_from_runtime_candidates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("Summarize README.md"))?;
    let mut runtime_context = RuntimeContextCandidates::new();
    let item = ContextItem {
        id: "repo-file:README.md".to_owned(),
        source: ContextSource::RepositoryFile,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: Some("snapshot-readme".to_owned()),
        token_cost: 4,
        score: Some(100.0),
        inclusion_reason: ContextInclusionReason::RetrievalHit,
        body_ref: ContextBodyRef::inline("Sigil readme context"),
    };
    runtime_context
        .snippets
        .insert(item.id.clone(), "Sigil readme context".to_owned());
    runtime_context.items.push(item);

    let request = session.build_request_with_transient_messages_and_context(
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
        &[],
        runtime_context,
    )?;

    let context_messages = request_context_v0_messages(&request);
    assert_eq!(context_messages.len(), 1);
    let context_text = context_messages[0]
        .content
        .as_deref()
        .expect("context content");
    assert!(context_text.contains("repo-file:README.md"));
    assert!(context_text.contains("repository_file"));
    assert!(context_text.contains("Sigil readme context"));
    assert!(context_text.contains("snapshot-readme"));
    let prefix = session.latest_prefix_snapshot().expect("prefix snapshot");
    assert!(prefix.materialized_text.contains("repo-file:README.md"));
    Ok(())
}

#[test]
fn build_request_refreshes_session_memory_snapshot_after_disk_memory_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    fs::write(temp.path().join("AGENTS.md"), "repo rules v1\n")?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let memory_config = MemoryConfig { enabled: true };

    session.append_user_message(ModelMessage::user("first"))?;
    let first = session.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    assert!(request_memory_text(&first).contains("repo rules v1"));

    fs::write(temp.path().join("AGENTS.md"), "repo rules v2\n")?;
    session.append_user_message(ModelMessage::user("second"))?;
    let second =
        session.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    let second_memory = request_memory_text(&second);
    assert!(second_memory.contains("repo rules v2"));
    assert!(!second_memory.contains("repo rules v1"));

    let fingerprints = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
                Some(snapshot.memory_fingerprint.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(fingerprints.len(), 2);
    assert_ne!(fingerprints[0], fingerprints[1]);
    assert_eq!(memory_snapshot_count(session.entries()), 2);

    session.append_user_message(ModelMessage::user("third"))?;
    let third = session.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    assert!(request_memory_text(&third).contains("repo rules v2"));
    assert_eq!(memory_snapshot_count(session.entries()), 2);

    let mut restored = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    restored.append_user_message(ModelMessage::user("after restore"))?;
    let restored_request =
        restored.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    let restored_memory = request_memory_text(&restored_request);
    assert!(restored_memory.contains("repo rules v2"));
    assert!(!restored_memory.contains("repo rules v1"));

    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert_eq!(memory_snapshot_count(&reloaded), 2);
    Ok(())
}

#[test]
fn messages_repair_orphan_tool_call_projection() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_user_message(ModelMessage::user("continue"))?;

    let projected = session.messages();

    assert_eq!(projected.len(), 3);
    assert!(matches!(projected[0].role, crate::MessageRole::Assistant));
    assert!(matches!(projected[1].role, crate::MessageRole::Tool));
    assert_eq!(projected[1].id, "local_repair:missing_tool_result:call-1");
    assert_eq!(projected[1].tool_call_id.as_deref(), Some("call-1"));
    assert!(projected[1].content.as_deref().is_some_and(|content| {
        content.contains("did not return a result before the previous run stopped")
            && content.contains(r#""kind":"interrupted""#)
    }));
    assert!(matches!(projected[2].role, crate::MessageRole::User));
    Ok(())
}

#[test]
fn load_from_store_marks_started_tool_execution_as_interrupted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }),
    )))?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Interrupted
                    && execution.error.as_ref().is_some_and(|error| {
                        error.kind == crate::ToolErrorKind::Interrupted && error.retryable
                    })
        )
    }));
    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Interrupted
        )
    }));
    Ok(())
}

#[test]
fn unfinished_write_tool_execution_profile_reconciles_workspace_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "old")?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let recorder = MutationEventRecorder::new(store.clone());
    let scope = VerificationScope::all_tracked("scope-main");
    let profile = recorder.execution_mutation_profile(
        &workspace,
        &scope,
        "call-shell",
        "bash",
        ToolEffect::Unknown,
    )?;
    let metadata = ToolResultMeta {
        details: serde_json::json!({
            "execution_mutation_profile": profile,
        }),
        ..Default::default()
    };
    store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-shell".to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata,
            error: None,
            model_content_hash: None,
        }),
    )))?;
    fs::write(workspace.join("note.txt"), "new")?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;

    let events = session.reconcile_unfinished_write_tool_executions(&workspace)?;

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].event_type,
        DurableEventType::WorkspaceMutationDetected.as_str()
    );
    let payload: WorkspaceMutationDetected = serde_json::from_value(events[0].payload.clone())?;
    assert_eq!(payload.tool_call_id.as_deref(), Some("call-shell"));
    assert_eq!(payload.tool_name, "bash");
    assert_eq!(payload.scope_hash, "scope-main");
    assert!(!payload.unknown_dirty);

    let duplicate = session.reconcile_unfinished_write_tool_executions(&workspace)?;
    assert!(duplicate.is_empty());
    Ok(())
}

#[test]
fn execution_mutation_profile_roundtrips_from_tool_metadata() -> Result<()> {
    let profile = ExecutionMutationProfile {
        tool_call_id: "call-shell".to_owned(),
        tool_name: "bash".to_owned(),
        effect: ToolEffect::Unknown,
        workspace_id: "workspace-1".to_owned(),
        scan_scope_hash: "scope-main".to_owned(),
        pre_execution_snapshot_id: Some("snapshot-before".to_owned()),
        pre_execution_workspace_revision: 7,
        workspace_knowledge: crate::WorkspaceKnowledge::Clean(7),
    };
    let metadata = ToolResultMeta {
        details: serde_json::json!({
            "execution_mutation_profile": profile,
        }),
        ..Default::default()
    };
    let restored: ExecutionMutationProfile =
        serde_json::from_value(metadata.details["execution_mutation_profile"].clone())?;

    assert_eq!(restored.tool_call_id, "call-shell");
    assert_eq!(restored.pre_execution_workspace_revision, 7);
    Ok(())
}

#[test]
fn latest_control_state_queries_return_latest_matching_records() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
        provider_name: "deepseek".to_owned(),
        response_id: "response-old".to_owned(),
        continuation_cursor: Some("cursor-old".to_owned()),
    }))?;
    session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
        provider_name: "other".to_owned(),
        response_id: "response-other".to_owned(),
        continuation_cursor: None,
    }))?;
    session.append_control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
        materialized_text: "prefix-old".to_owned(),
        sha256: "old".to_owned(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        memory_fingerprint: "memory-old".to_owned(),
        tool_schema_fingerprint: "tools-old".to_owned(),
        skill_index_fingerprint: "skills-old".to_owned(),
    }))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"old"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"new"}),
        },
    ))?;
    session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
        summary: "summary-old".to_owned(),
        compacted_message_count: 1,
        retained_tail_message_count: 2,
        task_memory: None,
    }))?;
    session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
        provider_name: "deepseek".to_owned(),
        response_id: "response-new".to_owned(),
        continuation_cursor: Some("cursor-new".to_owned()),
    }))?;
    session.append_control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
        materialized_text: "prefix-new".to_owned(),
        sha256: "new".to_owned(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        memory_fingerprint: "memory-new".to_owned(),
        tool_schema_fingerprint: "tools-new".to_owned(),
        skill_index_fingerprint: "skills-new".to_owned(),
    }))?;
    session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
        summary: "summary-new".to_owned(),
        compacted_message_count: 3,
        retained_tail_message_count: 2,
        task_memory: None,
    }))?;

    assert!(matches!(
        session.latest_response_handle("deepseek"),
        Some(handle) if handle.response_id == "response-new"
            && handle.continuation_cursor.as_deref() == Some("cursor-new")
    ));
    assert!(matches!(
        session.latest_response_handle("other"),
        Some(handle) if handle.response_id == "response-other"
    ));
    assert!(matches!(
        session.latest_prefix_snapshot(),
        Some(snapshot) if snapshot.sha256 == "new"
    ));
    assert!(matches!(
        session.latest_compaction_record(),
        Some(record) if record.summary == "summary-new"
    ));
    let states = session.continuation_states("deepseek");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].opaque_blob, serde_json::json!({"cursor":"new"}));
    Ok(())
}

#[test]
fn compaction_persists_record_and_projects_summary_plus_tail() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("step one"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("step two".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("step three"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("step four".to_owned()),
        Vec::new(),
    ))?;

    let record = session.compact_now(&CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(1000),
        tail_messages: 2,
    })?;

    assert_eq!(record.compacted_message_count, 2);
    assert_eq!(record.retained_tail_message_count, 2);
    assert!(session.entries().iter().any(|entry| {
        matches!(entry, SessionLogEntry::Control(ControlEntry::CompactionApplied(saved)) if saved == &record)
    }));

    let projected = session.messages();
    assert_eq!(projected.len(), 3);
    assert!(
        projected[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Compacted 2 earlier messages"))
    );
    assert_eq!(projected[1].content.as_deref(), Some("step three"));
    assert_eq!(projected[2].content.as_deref(), Some("step four"));
    Ok(())
}

#[test]
fn compaction_attaches_task_memory_from_durable_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_user_message(ModelMessage::user("step one"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("step two".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("step three"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("step four".to_owned()),
        Vec::new(),
    ))?;
    session.append_durable_event(
        DurableEventType::MutationCommitted,
        EventClass::Critical,
        serde_json::json!({
            "operation_id": "op-readme",
            "batch_id": null,
            "workspace_id": "workspace-1",
            "observed_after_hash": "sha256:after",
            "workspace_revision": 3,
            "workspace_snapshot_id": "snapshot-readme",
            "committed_subject": {
                "file": {
                    "path": "README.md",
                    "file_type": "file"
                }
            }
        }),
    )?;

    let record = session.compact_now(&CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(1000),
        tail_messages: 2,
    })?;

    let memory = record
        .task_memory
        .as_ref()
        .expect("durable mutation evidence should produce typed task memory");
    assert_eq!(memory.valid_for_snapshot, "snapshot-readme");
    assert!(memory.files_changed.iter().any(|file| {
        file.path == std::path::Path::new("README.md")
            && file.mutation_receipt_id.as_deref() == Some("op-readme")
    }));
    assert!(
        session
            .latest_compaction_record()
            .and_then(|saved| saved.task_memory)
            .is_some_and(|saved| saved.valid_for_snapshot == "snapshot-readme")
    );
    Ok(())
}

#[test]
fn can_compact_requires_a_safe_boundary() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "tool-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{\"path\":\"README.md\"}".to_owned(),
        }],
    ))?;
    session.append_tool_message(ModelMessage::tool("tool-1", "ok"))?;

    assert!(!session.can_compact(&CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(1000),
        tail_messages: 1,
    }));
    Ok(())
}

#[test]
fn compaction_preview_reports_folded_messages_and_projected_after_state() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("alpha"))?;
    session
        .append_assistant_message(ModelMessage::assistant(Some("beta".to_owned()), Vec::new()))?;
    session.append_user_message(ModelMessage::user("gamma"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("delta".to_owned()),
        Vec::new(),
    ))?;

    let preview = session
        .compaction_preview(&CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(1000),
            tail_messages: 2,
        })?
        .expect("preview should exist");

    assert_eq!(preview.record.compacted_message_count, 2);
    assert_eq!(preview.folded_messages.len(), 2);
    assert_eq!(preview.projected_messages.len(), 3);
    assert!(
        preview.projected_messages[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Compacted 2 earlier messages"))
    );
    assert_eq!(
        preview.projected_messages[1].content.as_deref(),
        Some("gamma")
    );
    assert_eq!(
        preview.projected_messages[2].content.as_deref(),
        Some("delta")
    );
    Ok(())
}

#[test]
fn load_from_store_accepts_legacy_pascal_case_control_entries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("legacy-session.jsonl");
    fs::write(
        &path,
        "{\"Control\":{\"SessionIdentity\":{\"provider_name\":\"deepseek\",\"model_name\":\"deepseek-v4-flash\"}}}\n",
    )?;

    let store = JsonlSessionStore::new(&path)?;
    let session = Session::load_from_store("fallback-provider", "fallback-model", store)?;

    assert_eq!(session.provider_name(), "deepseek");
    assert_eq!(session.model_name(), "deepseek-v4-flash");
    Ok(())
}

#[test]
fn session_stats_are_restored_from_usage_snapshots() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 120,
            completion_tokens: 10,
            cache_hit_tokens: 90,
            cache_miss_tokens: 30,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        })),
        SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 48,
            completion_tokens: 6,
            cache_hit_tokens: 28,
            cache_miss_tokens: 20,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        })),
        SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 2,
            retained_tail_message_count: 2,
            task_memory: None,
        })),
    ];

    let stats = session_stats_from_entries(&entries);
    let session = Session::from_entries("deepseek", "deepseek-v4-flash", entries);

    assert_eq!(stats.prompt_tokens, 168);
    assert_eq!(stats.last_prompt_tokens, 0);
    assert_eq!(session.stats().prompt_tokens, 168);
    assert_eq!(session.stats().last_prompt_tokens, 0);
    Ok(())
}

#[test]
fn usage_stats_projection_replays_mixed_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy_usage = SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
        prompt_tokens: 120,
        completion_tokens: 10,
        cache_hit_tokens: 90,
        cache_miss_tokens: 30,
        input_cost: 12.0,
        output_cost: 4.0,
        cache_savings: 7.0,
        system_fingerprint: None,
    }));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy_usage)?),
    )?;
    let store = JsonlSessionStore::new(&path)?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        UsageStats {
            prompt_tokens: 48,
            completion_tokens: 6,
            cache_hit_tokens: 28,
            cache_miss_tokens: 20,
            input_cost: 5.0,
            output_cost: 2.0,
            cache_savings: 3.0,
            system_fingerprint: None,
        },
    )))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::CompactionApplied(CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 2,
            retained_tail_message_count: 2,
            task_memory: None,
        }),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let stats = session
        .try_usage_stats_from_durable()?
        .expect("durable session should replay usage stats");

    assert_eq!(stats.prompt_tokens, 168);
    assert_eq!(stats.completion_tokens, 16);
    assert_eq!(stats.cache_hit_tokens, 118);
    assert_eq!(stats.cache_miss_tokens, 50);
    assert_eq!(stats.input_cost, 17.0);
    assert_eq!(stats.output_cost, 6.0);
    assert_eq!(stats.cache_savings, 10.0);
    assert_eq!(stats.last_prompt_tokens, 0);
    Ok(())
}

#[test]
fn usage_stats_projection_returns_none_without_store() -> Result<()> {
    let session = Session::new("deepseek", "deepseek-v4-flash");

    assert!(session.try_usage_stats_from_durable()?.is_none());
    Ok(())
}

#[test]
fn usage_stats_projection_record_helper_ignores_idempotent_replay() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        UsageStats {
            prompt_tokens: 9,
            completion_tokens: 3,
            cache_hit_tokens: 4,
            cache_miss_tokens: 5,
            input_cost: 0.9,
            output_cost: 0.3,
            cache_savings: 0.1,
            system_fingerprint: None,
        },
    )))?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let record = records
        .first()
        .expect("one durable record should be present");
    let mut stats = crate::provider::SessionStats::default();
    let mut cursor = None;

    super::apply_usage_projection_record(&mut stats, &mut cursor, record)?;
    super::apply_usage_projection_record(&mut stats, &mut cursor, record)?;

    assert_eq!(stats.prompt_tokens, 9);
    assert_eq!(stats.completion_tokens, 3);
    assert_eq!(stats.last_prompt_tokens, 9);
    assert!(cursor.is_some());
    Ok(())
}

#[test]
fn usage_stats_projection_record_helper_fails_closed_on_sequence_gap() -> Result<()> {
    let event = StoredEvent::new(
        DurableEventType::SessionEntryRecorded,
        EventClass::NonCritical,
        "event-2".to_owned(),
        "session-gap".to_owned(),
        2,
        serde_json::json!({}),
    )?;
    let record = SessionStreamRecord::Stored(event);
    let mut stats = crate::provider::SessionStats::default();
    let mut cursor = Some(ProjectionCursor {
        session_id: "session-gap".to_owned(),
        projection_schema_version: super::USAGE_STATE_PROJECTION_SCHEMA_VERSION,
        last_applied_stream_sequence: 0,
        last_applied_event_id: "event-0".to_owned(),
        last_applied_record_checksum: "sha256:0".to_owned(),
    });

    let error = super::apply_usage_projection_record(&mut stats, &mut cursor, &record)
        .expect_err("projection should fail closed on sequence gaps");

    assert!(error.to_string().contains("projection sequence gap"));
    assert_eq!(stats.prompt_tokens, 0);
    Ok(())
}

#[test]
fn usage_stats_projection_record_helper_fails_closed_on_unknown_critical_event() -> Result<()> {
    let event = StoredEvent::new_raw(
        "future_usage_event",
        EventClass::Critical,
        "event-future-usage".to_owned(),
        "session-usage".to_owned(),
        1,
        serde_json::json!({"value": "must not be ignored"}),
    )?;
    let record = SessionStreamRecord::Stored(event);
    let mut stats = crate::provider::SessionStats::default();
    let mut cursor = None;

    let error = super::apply_usage_projection_record(&mut stats, &mut cursor, &record)
        .expect_err("unknown critical event should fail closed");

    assert!(
        error
            .to_string()
            .contains("unknown critical event future_usage_event")
    );
    assert_eq!(stats.prompt_tokens, 0);
    assert!(cursor.is_none());
    Ok(())
}

#[test]
fn durable_projection_record_helpers_ignore_idempotent_replay() -> Result<()> {
    let event = StoredEvent::new(
        DurableEventType::DiagnosticRecorded,
        EventClass::Critical,
        "event-diagnostic".to_owned(),
        "session-projection".to_owned(),
        1,
        serde_json::json!({"message": "projection noop"}),
    )?;
    let record = SessionStreamRecord::Stored(event);

    let mut plan = crate::PlanApprovalProjection::default();
    let mut plan_cursor = None;
    super::apply_plan_approval_projection_record(&mut plan, &mut plan_cursor, &record)?;
    super::apply_plan_approval_projection_record(&mut plan, &mut plan_cursor, &record)?;

    let mut task = TaskStateProjection::default();
    let mut task_cursor = None;
    super::apply_task_projection_record(&mut task, &mut task_cursor, &record)?;
    super::apply_task_projection_record(&mut task, &mut task_cursor, &record)?;

    let mut skill = crate::SkillStateProjection::default();
    let mut skill_cursor = None;
    super::apply_skill_projection_record(&mut skill, &mut skill_cursor, &record)?;
    super::apply_skill_projection_record(&mut skill, &mut skill_cursor, &record)?;

    let mut plugin = crate::PluginStateProjection::default();
    let mut plugin_cursor = None;
    super::apply_plugin_projection_record(&mut plugin, &mut plugin_cursor, &record)?;
    super::apply_plugin_projection_record(&mut plugin, &mut plugin_cursor, &record)?;

    let mut agent_threads = AgentThreadStateProjection::default();
    let mut agent_threads_cursor = None;
    super::apply_agent_thread_projection_record(
        &mut agent_threads,
        &mut agent_threads_cursor,
        &record,
    )?;
    super::apply_agent_thread_projection_record(
        &mut agent_threads,
        &mut agent_threads_cursor,
        &record,
    )?;

    let mut agent_trust = crate::AgentProfileTrustProjection::default();
    let mut agent_trust_cursor = None;
    super::apply_agent_profile_trust_projection_record(
        &mut agent_trust,
        &mut agent_trust_cursor,
        &record,
    )?;
    super::apply_agent_profile_trust_projection_record(
        &mut agent_trust,
        &mut agent_trust_cursor,
        &record,
    )?;

    let mut agent_policy = crate::AgentProfilePolicyProjection::default();
    let mut agent_policy_cursor = None;
    super::apply_agent_profile_policy_projection_record(
        &mut agent_policy,
        &mut agent_policy_cursor,
        &record,
    )?;
    super::apply_agent_profile_policy_projection_record(
        &mut agent_policy,
        &mut agent_policy_cursor,
        &record,
    )?;

    let mut agent_results = crate::AgentResultContinuationProjection::default();
    let mut agent_results_cursor = None;
    super::apply_agent_result_continuation_projection_record(
        &mut agent_results,
        &mut agent_results_cursor,
        &record,
    )?;
    super::apply_agent_result_continuation_projection_record(
        &mut agent_results,
        &mut agent_results_cursor,
        &record,
    )?;

    let mut conversation = crate::ConversationQueueProjection::default();
    let mut conversation_cursor = None;
    super::apply_conversation_queue_projection_record(
        &mut conversation,
        &mut conversation_cursor,
        &record,
    )?;
    super::apply_conversation_queue_projection_record(
        &mut conversation,
        &mut conversation_cursor,
        &record,
    )?;

    let mut changeset = crate::ChangeSetProjection::default();
    let mut changeset_cursor = None;
    super::apply_changeset_projection_record(&mut changeset, &mut changeset_cursor, &record)?;
    super::apply_changeset_projection_record(&mut changeset, &mut changeset_cursor, &record)?;

    let mut terminal = crate::TerminalTaskProjection::default();
    let mut terminal_cursor = None;
    super::apply_terminal_task_projection_record(&mut terminal, &mut terminal_cursor, &record)?;
    super::apply_terminal_task_projection_record(&mut terminal, &mut terminal_cursor, &record)?;

    for cursor in [
        plan_cursor,
        task_cursor,
        skill_cursor,
        plugin_cursor,
        agent_threads_cursor,
        agent_trust_cursor,
        agent_policy_cursor,
        agent_results_cursor,
        conversation_cursor,
        changeset_cursor,
        terminal_cursor,
    ] {
        assert_eq!(
            cursor
                .expect("cursor should be recorded")
                .last_applied_stream_sequence,
            1
        );
    }
    Ok(())
}

#[test]
fn continuation_states_keep_latest_state_per_key_and_provider() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "cursor".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"old"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "cursor".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"new"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "other".to_owned(),
            state_kind: "cursor".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"other"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: None,
            opaque_blob: serde_json::json!({"trace":"kept"}),
        },
    ))?;

    let mut states = session.continuation_states("deepseek");
    states.sort_by(|left, right| left.state_kind.cmp(&right.state_kind));

    assert_eq!(states.len(), 2);
    assert_eq!(states[0].state_kind, "cursor");
    assert_eq!(states[0].opaque_blob["cursor"], "new");
    assert_eq!(states[1].state_kind, "reasoning");
    Ok(())
}

#[test]
fn build_request_only_includes_matching_provider_continuation_states() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("hello"))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"keep"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "other-provider".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-2".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"drop"}),
        },
    ))?;

    let request = session.build_request(
        std::env::temp_dir().as_path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
    )?;

    assert_eq!(request.continuation_states.len(), 1);
    assert_eq!(request.continuation_states[0].provider_name, "deepseek");
    assert_eq!(
        request.continuation_states[0].opaque_blob,
        serde_json::json!({"cursor":"keep"})
    );
    Ok(())
}

#[test]
fn ensure_identity_entry_is_idempotent() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");

    session.ensure_identity_entry()?;
    session.ensure_identity_entry()?;

    let identity_entries = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            )
        })
        .count();
    assert_eq!(identity_entries, 1);
    Ok(())
}

#[test]
fn compaction_preview_returns_none_for_insufficient_history() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("only one"))?;

    let preview = session.compaction_preview(&CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(1000),
        tail_messages: 2,
    })?;

    assert!(preview.is_none());
    Ok(())
}

#[test]
fn compact_now_rejects_disabled_config() {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");

    let error = session
        .compact_now(&CompactionConfig {
            enabled: false,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(1000),
            tail_messages: 2,
        })
        .expect_err("disabled compaction should fail");

    assert!(error.to_string().contains("compaction is disabled"));
}

#[test]
fn load_from_store_does_not_duplicate_closed_tool_execution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }),
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(12),
            subjects: Vec::new(),
            changed_files: vec!["file.txt".to_owned()],
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: Some("hash".to_owned()),
        }),
    )))?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let interrupted_count = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.call_id == "call-1"
                        && execution.status == ToolExecutionStatus::Interrupted
            )
        })
        .count();

    assert_eq!(interrupted_count, 0);
    Ok(())
}

#[test]
fn jsonl_session_store_ignores_blank_lines_and_reports_parse_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(
        &path,
        format!(
            "\n{}\nnot-json\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("hello")))?,
        ),
    )?;

    let error = JsonlSessionStore::read_entries(&path).expect_err("invalid json should fail");
    assert!(error.to_string().contains("line 3"));
    assert!(error.to_string().contains("session.jsonl"));

    fs::write(
        &path,
        format!(
            "\n{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("hello")))?,
        ),
    )?;
    let entries = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(entries.len(), 1);
    Ok(())
}

#[test]
fn session_compaction_helpers_cover_disabled_and_insufficient_history_paths() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let disabled = CompactionConfig {
        enabled: false,
        ..CompactionConfig::default()
    };
    let enabled = CompactionConfig {
        enabled: true,
        ..CompactionConfig::default()
    };

    assert!(!session.can_compact(&disabled));
    assert!(
        session
            .compact_now(&disabled)
            .expect_err("disabled compaction should fail")
            .to_string()
            .contains("disabled")
    );
    assert!(
        session
            .compaction_preview(&disabled)
            .expect_err("disabled compaction preview should fail")
            .to_string()
            .contains("disabled")
    );
    assert!(session.compaction_preview(&enabled)?.is_none());

    session.append_user_message(ModelMessage::user("hello"))?;
    assert!(
        session
            .compact_now(&enabled)
            .expect_err("single-message history should be insufficient")
            .to_string()
            .contains("enough history")
    );
    assert!(!session.can_compact(&enabled));

    assert_eq!(session.store_path(), None);
    session.stats_mut().last_prompt_tokens = 9;
    assert_eq!(session.stats().last_prompt_tokens, 9);
    Ok(())
}

#[test]
fn session_projection_helpers_repair_orphans_and_ignore_empty_compaction_records() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
        summary: "   ".to_owned(),
        compacted_message_count: 1,
        retained_tail_message_count: 1,
        task_memory: None,
    }))?;

    let projected = session.messages();
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[1].tool_call_id.as_deref(), Some("call-1"));

    let summary = super::compaction_summary_message(&CompactionRecord {
        summary: "summary".to_owned(),
        compacted_message_count: 2,
        retained_tail_message_count: 1,
        task_memory: None,
    });
    assert_eq!(summary.role, crate::MessageRole::Assistant);
    assert_eq!(summary.content.as_deref(), Some("summary"));
    assert!(summary.id.starts_with("compaction:"));
    Ok(())
}

#[test]
fn session_boundary_summary_and_identity_helpers_cover_tool_edges() {
    assert_eq!(super::compaction_boundary(&[], 2), 0);

    let assistant_call = ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    let tool_message = ModelMessage::tool("call-1", "result");
    let user_message = ModelMessage::user("follow up");
    let boundary =
        super::compaction_boundary(&[assistant_call.clone(), tool_message, user_message], 1);
    assert_eq!(boundary, 0);

    let summary = super::summarize_messages(&[
        ModelMessage::system("system prompt"),
        ModelMessage::user("hello\nworld"),
        assistant_call.clone(),
        ModelMessage::assistant(
            Some("checking provider shape".to_owned()),
            vec![crate::ToolCall {
                id: "call-2".to_owned(),
                name: "grep".to_owned(),
                args_json: "{}".to_owned(),
            }],
        ),
        ModelMessage {
            id: "tool-no-id".to_owned(),
            role: crate::MessageRole::Tool,
            content: Some("content".repeat(80)),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
    ]);
    assert!(summary.contains("01. system system prompt"));
    assert!(summary.contains("03. assistant tool_calls [read_file]"));
    assert!(summary.contains("04. assistant checking provider shape tool_calls [grep]"));
    assert!(summary.contains("05. tool unknown =>"));
    assert!(summary.contains("..."));

    assert_eq!(super::truncate_stable("a   b", 10), "a b");
    assert!(super::truncate_stable(&"x".repeat(200), 12).ends_with("..."));

    let identity_entries = vec![SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4".to_owned(),
    })];
    assert_eq!(
        super::session_identity_from_entries(&identity_entries),
        Some(("deepseek".to_owned(), "deepseek-v4".to_owned()))
    );
}

#[test]
fn interrupted_tool_executions_only_keep_open_started_records() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "open".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: Some(5),
            subjects: Vec::new(),
            changed_files: vec!["note.txt".to_owned()],
            metadata: ToolResultMeta {
                changed_files: vec!["note.txt".to_owned()],
                ..ToolResultMeta::default()
            },
            error: None,
            model_content_hash: Some("hash".to_owned()),
        }))),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "done".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }))),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "done".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(1),
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: Some("done".to_owned()),
        }))),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "cancelled".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Cancelled,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }))),
    ];

    let interrupted = super::interrupted_tool_executions(&entries);
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].call_id, "open");
    assert_eq!(interrupted[0].status, ToolExecutionStatus::Interrupted);
    assert!(interrupted[0].changed_files.is_empty());
    assert!(interrupted[0].metadata.changed_files.is_empty());
    assert!(interrupted[0].error.is_some());
    assert_eq!(interrupted[0].model_content_hash, None);
}
