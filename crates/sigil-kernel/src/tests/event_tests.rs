use serde_json::json;

use crate::{
    ALL_DURABLE_EVENT_TYPES, AgentApprovalRouteEntry, AgentElicitationRouteEntry,
    AgentInvocationMode, AgentInvocationSource, AgentMergeSafePointEntry,
    AgentProfileCapturedEntry, AgentProfileId, AgentProfilePolicyEntry, AgentProfileSnapshot,
    AgentProfileSnapshotId, AgentProfileSource, AgentProfileTrustEntry,
    AgentResultContinuationEntry, AgentResultContinuationStatus, AgentRole, AgentRouteClosedEntry,
    AgentRouteId, AgentRouteStatus, AgentRunAttemptId, AgentRunAttemptStartedEntry,
    AgentRunContextSnapshot, AgentRunHeartbeatEntry, AgentRunInterruptedEntry,
    AgentThreadClosedEntry, AgentThreadDisplayNameEntry, AgentThreadId,
    AgentThreadMessageRoutedEntry, AgentThreadResult, AgentThreadResultRecordedEntry,
    AgentThreadStartedEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
    AgentThreadTerminalStatus, AgentTrustState, ApprovalMode, BackgroundTaskHandle, CandidateCheck,
    ChangeSet, ChangeSetId, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk, CheckCommand,
    CheckDiscoverySource, CheckPromotion, CheckSpec, CheckSpecRecordedEntry,
    ChildVerificationReceiptLinked, CompactionRecord, CompletionCriteria, ControlEntry,
    ConversationInputEditedEntry, ConversationInputKind, ConversationInputQueueControlAction,
    ConversationInputQueueControlEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputReorderedEntry, ConversationInputStatus, ConversationInputStatusEntry,
    ConversationInputTarget, DurableDomainEvent, DurableEventType, EventClass, EventSyncClass,
    EvidenceReceipt, EvidenceScope, LegacyEvent, MAX_EVENT_BYTES, MAX_PAYLOAD_DEPTH,
    McpElicitationDecision, McpElicitationEntry, MemoryLoadReport, MemorySnapshot, ModelMessage,
    PUBLIC_RUN_EVENT_SCHEMA_VERSION, PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalScope,
    PlanApprovedEntry, PluginCapability, PluginManifestSnapshot, PluginTrustDecision,
    PluginTrustEntry, PrefixSnapshot, ProjectionApplyDecision, ProjectionCursor,
    ProviderContinuationState, PublicControlEvent, PublicRunEvent, PublicRunEventKind,
    ReadinessEvaluatedEntry, ReadinessEvaluation, ReasoningEffort, ReceiptStatus, RedactionState,
    ReducerDisposition, RequiredAction, ResponseHandle, RunEvent, RunStatus,
    SandboxProfileRequirement, SessionLogEntry, SessionRef, SkillDescriptor, SkillIndexSnapshot,
    SkillLoadEntry, SkillRunMode, SkillSource, SkillTrustState, StoredEvent, StoredEventDecode,
    TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskChildSessionStatus, TaskId,
    TaskPlanEntry, TaskPlanStatus, TaskRouteId, TaskRouteStatus, TaskRunEntry, TaskRunStatus,
    TaskStepEntry, TaskStepId, TaskStepStatus, TaskSubagentApprovalRouteEntry,
    TaskSubagentElicitationRouteEntry, TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId,
    TerminalTaskStatus, ToolAccess, ToolApprovalAuditAction, ToolApprovalEntry, ToolCall,
    ToolCategory, ToolEffect, ToolEgressEntry, ToolExecutionEntry, ToolExecutionStatus,
    ToolPreview, ToolPreviewCapability, ToolPreviewFile, ToolPreviewSnapshot, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject, TypedDomainEvent, TypedStoredEventDecode, UsageStats,
    VerificationAutoRunPolicy, VerificationBinding, VerificationCheckRunEntry,
    VerificationCheckRunStatus, VerificationPolicy, VerificationPolicyChangedEntry,
    VerificationReceipt, VerificationRecordedEntry, VerificationScope, VerificationVerdict,
    VisibleCompletionState, WorkspaceMutationDetected, WorkspaceMutationDetectionReason,
    WorkspaceRootSnapshot, WorkspaceTrust, WorkspaceTrustDecisionEntry, WorkspaceTrustRequirement,
    decode_stored_event, decode_typed_stored_event, is_transient_run_event,
    projection_apply_decision, projection_apply_decision_for_record, reducer_disposition,
};

#[test]
fn stored_event_checksum_is_canonical_and_roundtrips() {
    let event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        json!({
            "z": "line\nquoted \"text\"",
            "a": {
                "unicode": "sigil-λ",
                "number": 1.0,
                "items": [true, null, 3]
            }
        }),
    )
    .expect("stored event should be valid");

    let line = event.to_json_line().expect("stored event should serialize");
    let parsed = StoredEvent::from_json_str(&line).expect("stored event should parse");

    assert_eq!(parsed.record_checksum, event.record_checksum);
    assert!(parsed.record_checksum.starts_with("sha256:jcs-v1:"));
    assert_eq!(parsed, event);
}

#[test]
fn stored_event_new_rejects_non_appendable_legacy_event_type() {
    let error = StoredEvent::new(
        DurableEventType::Legacy,
        EventClass::NonCritical,
        "event-legacy".to_owned(),
        "session-1".to_owned(),
        1,
        json!({}),
    )
    .expect_err("legacy envelopes are replay-only and cannot be appended");

    assert!(error.to_string().contains("cannot be appended"));
}

#[test]
fn stored_event_checksum_normalizes_numeric_integer_forms() {
    let integer_payload =
        serde_json::from_str(r#"{"a":{"number":1}}"#).expect("integer json parses");
    let float_payload = serde_json::from_str(r#"{"a":{"number":1.0}}"#).expect("float json parses");

    let integer_event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        integer_payload,
    )
    .expect("stored event should be valid");
    let float_event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        float_payload,
    )
    .expect("stored event should be valid");

    assert_eq!(integer_event.record_checksum, float_event.record_checksum);
    assert!(
        StoredEvent::from_json_str(&float_event.to_json_line().expect("event serializes")).is_ok()
    );
}

#[test]
fn stored_event_checksum_accepts_usage_snapshot_float_fixture() {
    let line = r#"{"schema_version":1,"event_type":"session_entry_recorded","event_version":1,"event_class":"non_critical","event_id":"2564cd0d-285c-5d88-9395-7fd7858fc242","session_id":"35181898-105b-554f-985d-209212d5f4b3","stream_sequence":64,"record_checksum":"sha256:jcs-v1:3f454890559a65234d6bf2315970303276284990bde7a7147789a113acc1c039","payload":{"session_log_entry":{"control":{"usage_snapshot":{"cache_hit_tokens":7296,"cache_miss_tokens":13325,"cache_savings":0.0031473119999999998,"completion_tokens":1015,"input_cost":0.005822823,"output_cost":0.0008830499999999999,"prompt_tokens":20621,"system_fingerprint":"fp_9954b31ca7_prod0820_fp8_kvcache_20260402"}}}}}"#;
    let event_without_verification = StoredEvent::from_value(
        serde_json::from_str(line).expect("usage snapshot fixture should parse"),
    )
    .expect("usage snapshot fixture envelope should deserialize");

    assert_eq!(
        event_without_verification
            .compute_record_checksum()
            .expect("fixture checksum should compute"),
        event_without_verification.record_checksum
    );
    let parsed =
        StoredEvent::from_json_str(line).expect("usage snapshot fixture checksum should verify");

    assert_eq!(parsed.stream_sequence, 64);
    assert_eq!(parsed.event_class, EventClass::NonCritical);
    assert_eq!(
        parsed.record_checksum,
        "sha256:jcs-v1:3f454890559a65234d6bf2315970303276284990bde7a7147789a113acc1c039"
    );
}

#[test]
fn durable_event_sync_class_identifies_normal_events() {
    assert_eq!(
        DurableEventType::UserMessageRecorded
            .sync_class()
            .expect("user message is appendable"),
        EventSyncClass::NormalEvent
    );
    assert_eq!(
        DurableEventType::AssistantMessageRecorded
            .sync_class()
            .expect("assistant message is appendable"),
        EventSyncClass::NormalEvent
    );
    assert_eq!(
        DurableEventType::ContextSourceCaptured
            .sync_class()
            .expect("context source is appendable"),
        EventSyncClass::NormalEvent
    );
}

#[test]
fn stored_event_rejects_checksum_mismatch_and_bad_json_separately() {
    let mut event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"tool": "read_file"}),
    )
    .expect("stored event should be valid");
    event.record_checksum = "sha256:jcs-v1:bad".to_owned();
    let line = serde_json::to_string(&event).expect("event serializes");

    let checksum_error =
        StoredEvent::from_json_str(&line).expect_err("checksum mismatch should fail");
    assert!(checksum_error.to_string().contains("checksum mismatch"));

    let parse_error =
        StoredEvent::from_json_str("{not-json").expect_err("invalid json should fail");
    assert!(parse_error.to_string().contains("parse stored event json"));
}

#[test]
fn stored_event_unknown_class_rules_fail_closed_when_required() {
    let unknown = StoredEvent::new_raw(
        "new_noncritical_event",
        EventClass::NonCritical,
        "event-unknown".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"value": 1}),
    )
    .expect("non-critical unknown event should serialize");
    let decoded = decode_stored_event(unknown).expect("non-critical unknown should decode");
    assert!(matches!(decoded, StoredEventDecode::UnknownNonCritical(_)));

    let critical = StoredEvent::new_raw(
        "new_critical_event",
        EventClass::Critical,
        "event-critical".to_owned(),
        "session-1".to_owned(),
        2,
        json!({"value": 1}),
    )
    .expect("critical unknown event should serialize");
    let error = decode_stored_event(critical).expect_err("critical unknown should fail");
    assert!(error.to_string().contains("unknown critical event"));

    let missing_class = json!({
        "schema_version": 1,
        "event_type": "new_event",
        "event_version": 1,
        "event_id": "event-missing-class",
        "session_id": "session-1",
        "stream_sequence": 3,
        "record_checksum": "sha256:jcs-v1:missing",
        "payload": {}
    });
    let error =
        StoredEvent::from_value(missing_class).expect_err("missing event_class should fail closed");
    assert!(error.to_string().contains("event_class"));
}

#[test]
fn typed_stored_event_decode_handles_unknown_and_legacy_boundaries() {
    let unknown = StoredEvent::new_raw(
        "new_noncritical_event",
        EventClass::NonCritical,
        "event-unknown-typed".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"value": 1}),
    )
    .expect("non-critical unknown event should serialize");
    let decoded = decode_typed_stored_event(unknown).expect("non-critical unknown should decode");
    assert!(matches!(
        decoded,
        TypedStoredEventDecode::UnknownNonCritical(_)
    ));

    let critical = StoredEvent::new_raw(
        "new_critical_event",
        EventClass::Critical,
        "event-critical-typed".to_owned(),
        "session-1".to_owned(),
        2,
        json!({"value": 1}),
    )
    .expect("critical unknown event should serialize");
    let error = decode_typed_stored_event(critical).expect_err("critical unknown should fail");
    assert!(error.to_string().contains("unknown critical event"));

    let legacy = StoredEvent::new_raw(
        DurableEventType::Legacy.as_str(),
        EventClass::Critical,
        "event-legacy-typed".to_owned(),
        "session-1".to_owned(),
        3,
        json!({}),
    )
    .expect("legacy event envelope can be constructed for decode validation");
    let error = decode_typed_stored_event(legacy).expect_err("legacy should not decode from v2");
    assert!(error.to_string().contains("upcast-only"));
}

#[test]
fn stored_event_rejects_missing_event_type_and_unknown_critical_on_wire() {
    let missing_event_type = json!({
        "schema_version": 1,
        "event_class": "critical",
        "event_version": 1,
        "event_id": "event-missing-type",
        "session_id": "session-1",
        "stream_sequence": 1,
        "record_checksum": "sha256:jcs-v1:missing",
        "payload": {}
    });
    let missing_error = StoredEvent::from_value(missing_event_type)
        .expect_err("missing event_type should fail closed");
    assert!(missing_error.to_string().contains("event_type"));

    let critical = StoredEvent::new_raw(
        "new_critical_event",
        EventClass::Critical,
        "event-critical".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"value": 1}),
    )
    .expect("critical unknown event can be serialized by a newer writer");
    let critical_error = StoredEvent::from_json_str(
        &critical
            .to_json_line()
            .expect("critical unknown event serializes"),
    )
    .expect_err("unknown critical event should fail closed");
    assert!(
        critical_error
            .to_string()
            .contains("unknown critical event")
    );
}

#[test]
fn stored_event_rejects_unsupported_schema_and_known_event_versions() {
    let mut schema_event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-schema".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"tool": "read_file"}),
    )
    .expect("stored event should be valid");
    schema_event.schema_version += 1;
    schema_event.record_checksum = schema_event
        .compute_record_checksum()
        .expect("checksum can be recomputed");
    let schema_error = StoredEvent::from_json_str(
        &schema_event
            .to_json_line()
            .expect("event should serialize with updated checksum"),
    )
    .expect_err("unsupported schema version should fail closed");
    assert!(schema_error.to_string().contains("schema_version"));

    let mut event_version = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-version".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"tool": "read_file"}),
    )
    .expect("stored event should be valid");
    event_version.event_version += 1;
    event_version.record_checksum = event_version
        .compute_record_checksum()
        .expect("checksum can be recomputed");
    let version_error = StoredEvent::from_json_str(
        &event_version
            .to_json_line()
            .expect("event should serialize with updated checksum"),
    )
    .expect_err("unsupported event version should fail closed");
    assert!(version_error.to_string().contains("event_version"));
}

#[test]
fn stored_event_decode_returns_strong_domain_event_variant() {
    let event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-typed".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"tool": "read_file"}),
    )
    .expect("stored event should be valid");

    let decoded = decode_stored_event(event).expect("known event should decode");

    let StoredEventDecode::Known(domain_event) = decoded else {
        panic!("known event should decode to domain event");
    };
    assert_eq!(
        domain_event.event_type(),
        DurableEventType::ToolExecutionStarted
    );
}

#[test]
fn typed_event_decode_covers_mutation_and_verification_family() {
    let prepared = StoredEvent::new(
        DurableEventType::MutationPrepared,
        EventClass::Critical,
        "event-mutation-prepared".to_owned(),
        "session-1".to_owned(),
        1,
        json!({
            "operation_id": "op-1",
            "tool_call_id": "tool-call-1",
            "causation_event_id": "event-tool-started",
            "subject": { "file": { "path": "src/lib.rs", "file_type": "file" } },
            "before_hash": "sha256:before",
            "intended_after_hash": "sha256:after",
            "snapshot_coverage": { "no_prior_content": null },
            "workspace_id": "workspace-1",
            "base_workspace_revision": 1,
            "sync_class": "recovery_critical"
        }),
    )
    .expect("mutation prepared event should build");

    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(prepared).expect("typed mutation should decode")
    else {
        panic!("expected typed mutation prepared");
    };
    let TypedDomainEvent::MutationPrepared(payload) = *event else {
        panic!("expected typed mutation prepared");
    };
    assert_eq!(payload.operation_id, "op-1");

    let check = event_check_spec();
    let run = VerificationCheckRunEntry::new(
        "run-1".to_owned(),
        EvidenceScope::Task("task-1".to_owned()),
        &check,
        VerificationCheckRunStatus::Queued,
    );
    let run_event = stored_control_event(
        DurableEventType::VerificationCheckRun,
        ControlEntry::VerificationCheckRun(run),
        2,
    );

    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(run_event).expect("typed check run should decode")
    else {
        panic!("expected typed verification check run");
    };
    let TypedDomainEvent::VerificationCheckRun(run) = *event else {
        panic!("expected typed verification check run");
    };
    assert_eq!(run.run_id, "run-1");

    let receipt = VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id: "receipt-1".to_owned(),
            source_session_id: "session-1".to_owned(),
            source_event_id: "event-check".to_owned(),
            source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
            scope: EvidenceScope::Task("task-1".to_owned()),
            producer_tool_call: None,
            workspace_revision: Some(1),
            workspace_snapshot_id: Some("snapshot-1".to_owned()),
            policy_hash: Some("policy-1".to_owned()),
            changeset_id: None,
            status: ReceiptStatus::Succeeded,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence: 2,
        },
        check_spec_id: check.check_spec_id,
        check_status: ReceiptStatus::Succeeded,
        binding: VerificationBinding {
            workspace_id: "workspace-1".to_owned(),
            workspace_snapshot_id: "snapshot-1".to_owned(),
            verification_scope_hash: "scope-main".to_owned(),
            check_spec_hash: "check-hash".to_owned(),
            environment_fingerprint: "env-1".to_owned(),
            sandbox_profile_hash: "sandbox-1".to_owned(),
            execution_backend: None,
            execution_backend_capabilities: None,
            workspace_trust_snapshot_id: "trust-1".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        failure_reason: None,
        mutates_verification_scope: false,
    };
    let recorded_event = stored_control_event(
        DurableEventType::VerificationRecorded,
        ControlEntry::VerificationRecorded(VerificationRecordedEntry { receipt }),
        3,
    );

    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(recorded_event).expect("typed receipt should decode")
    else {
        panic!("expected typed verification receipt");
    };
    let TypedDomainEvent::VerificationRecorded(recorded) = *event else {
        panic!("expected typed verification receipt");
    };
    assert_eq!(recorded.receipt.receipt.receipt_id, "receipt-1");

    let detected = WorkspaceMutationDetected {
        operation_id: "op-detected".to_owned(),
        tool_call_id: Some("tool-call-bash".to_owned()),
        tool_name: "bash".to_owned(),
        tool_effect: ToolEffect::Unknown,
        workspace_id: "workspace-1".to_owned(),
        scope_hash: "scope-main".to_owned(),
        from_workspace_snapshot_id: Some("snapshot-before".to_owned()),
        to_workspace_snapshot_id: None,
        base_workspace_revision: 3,
        workspace_revision: 4,
        reason: WorkspaceMutationDetectionReason::ScanUnavailable,
        unknown_dirty: true,
    };
    let detected_event = StoredEvent::new(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        "event-workspace-mutation".to_owned(),
        "session-1".to_owned(),
        4,
        serde_json::to_value(&detected).expect("workspace mutation serializes"),
    )
    .expect("workspace mutation event should build");
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(detected_event).expect("workspace mutation should decode")
    else {
        panic!("expected typed workspace mutation detected");
    };
    let TypedDomainEvent::WorkspaceMutationDetected(payload) = *event else {
        panic!("expected typed workspace mutation detected");
    };
    assert_eq!(payload.operation_id, "op-detected");
}

#[test]
fn typed_event_decode_covers_task_agent_terminal_and_changeset_family() {
    let task_event = stored_control_event(
        DurableEventType::TaskStatusChanged,
        ControlEntry::TaskRun(task_run_entry()),
        1,
    );
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(task_event).expect("task event decodes")
    else {
        panic!("expected typed task event");
    };
    assert!(matches!(
        *event,
        TypedDomainEvent::TaskStatusChanged(ControlEntry::TaskRun(_))
    ));

    let agent_event = stored_control_event(
        DurableEventType::SessionEntryRecorded,
        ControlEntry::AgentThreadStarted(AgentThreadStartedEntry {
            thread_id: agent_thread_id(),
            parent_thread_id: Some(AgentThreadId::new("main").expect("valid thread id")),
            parent_session_ref: session_ref(),
            thread_session_ref: agent_session_ref(),
            profile_id: agent_profile_id(),
            profile_snapshot_id: agent_snapshot_id(),
            run_context: agent_run_context(),
            objective: "inspect kernel".to_owned(),
            prompt_hash: "sha256:prompt".to_owned(),
            invocation_mode: AgentInvocationMode::Foreground,
            invocation_source: AgentInvocationSource::Chat,
            display_name: Some("kernel map".to_owned()),
            created_at_ms: Some(42),
        }),
        2,
    );
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(agent_event).expect("agent event decodes")
    else {
        panic!("expected typed agent event");
    };
    assert!(matches!(
        *event,
        TypedDomainEvent::AgentThread(ControlEntry::AgentThreadStarted(_))
    ));

    let terminal_event = stored_control_event(
        DurableEventType::SessionEntryRecorded,
        ControlEntry::TerminalTask(sample_terminal_task_entry()),
        3,
    );
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(terminal_event).expect("terminal event decodes")
    else {
        panic!("expected typed terminal event");
    };
    assert!(matches!(*event, TypedDomainEvent::TerminalTask(_)));

    let changeset_event = stored_control_event(
        DurableEventType::SessionEntryRecorded,
        ControlEntry::ChangeSetProposed(sample_change_set()),
        4,
    );
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(changeset_event).expect("changeset event decodes")
    else {
        panic!("expected typed changeset event");
    };
    assert!(matches!(*event, TypedDomainEvent::ChangeSetProposed(_)));

    let changeset_applied_event = stored_control_event(
        DurableEventType::SessionEntryRecorded,
        ControlEntry::ChangeSetApplied(ChangeSetResult {
            id: ChangeSetId::new("change-1").expect("valid change set id"),
            status: ChangeSetResultStatus::Applied,
            file_results: Vec::new(),
            message: None,
        }),
        5,
    );
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(changeset_applied_event).expect("changeset result decodes")
    else {
        panic!("expected typed changeset applied");
    };
    assert!(matches!(
        *event,
        TypedDomainEvent::ChangeSetApplied(ChangeSetResult {
            status: ChangeSetResultStatus::Applied,
            ..
        })
    ));

    let check = event_check_spec();
    let run = VerificationCheckRunEntry::new(
        "run-non-task".to_owned(),
        EvidenceScope::Task("task-1".to_owned()),
        &check,
        VerificationCheckRunStatus::Queued,
    );
    let non_task_status_event = stored_control_event(
        DurableEventType::TaskStatusChanged,
        ControlEntry::VerificationCheckRun(run),
        6,
    );
    let error = decode_typed_stored_event(non_task_status_event)
        .expect_err("task status event should reject non-task control payload");
    assert!(error.to_string().contains("non-task control payload"));
}

#[test]
fn typed_event_decode_covers_other_event_fallbacks() {
    let command_event = StoredEvent::new(
        DurableEventType::CommandFinished,
        EventClass::Critical,
        "event-command-finished".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"command": "cargo test"}),
    )
    .expect("command finished event should build");
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(command_event).expect("command event should decode")
    else {
        panic!("expected typed other event");
    };
    assert!(matches!(
        *event,
        TypedDomainEvent::Other(DurableDomainEvent::CommandFinished(_))
    ));

    let session_entry_without_control = StoredEvent::new(
        DurableEventType::SessionEntryRecorded,
        EventClass::NonCritical,
        "event-session-entry-other".to_owned(),
        "session-1".to_owned(),
        2,
        json!({"not_session_log_entry": true}),
    )
    .expect("session entry event should build");
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(session_entry_without_control)
            .expect("session entry fallback should decode")
    else {
        panic!("expected typed other event");
    };
    assert!(matches!(
        *event,
        TypedDomainEvent::Other(DurableDomainEvent::SessionEntryRecorded(_))
    ));
}

#[test]
fn stored_event_decode_rejects_legacy_stored_event() {
    let event = StoredEvent::new_raw(
        DurableEventType::Legacy.as_str(),
        EventClass::Critical,
        "event-legacy".to_owned(),
        "session-1".to_owned(),
        1,
        json!({}),
    )
    .expect("legacy event envelope can be constructed for decode validation");

    let error = decode_stored_event(event).expect_err("legacy event should not decode from v2");

    assert!(error.to_string().contains("upcast-only"));
}

#[test]
fn stored_event_decode_covers_every_known_domain_variant() {
    for event_type in ALL_DURABLE_EVENT_TYPES
        .iter()
        .copied()
        .filter(|event_type| *event_type != DurableEventType::Legacy)
    {
        let event_class = event_type
            .expected_event_class()
            .expect("known durable event type should have expected class");
        let event = StoredEvent::new(
            event_type,
            event_class,
            format!("event-{}", event_type.as_str()),
            "session-1".to_owned(),
            1,
            json!({"event_type": event_type.as_str()}),
        )
        .expect("stored event should be valid");

        let decoded = decode_stored_event(event).expect("known event should decode");

        let StoredEventDecode::Known(domain_event) = decoded else {
            panic!("known event should decode to domain event");
        };
        assert_eq!(domain_event.event_type(), event_type);
        assert_eq!(
            domain_event
                .payload()
                .and_then(|payload| payload.payload.get("event_type"))
                .and_then(|value| value.as_str()),
            Some(event_type.as_str())
        );
    }
    assert!(
        DurableDomainEvent::Legacy(LegacyEvent {
            event_id: "event-legacy".to_owned(),
            session_id: "session-legacy".to_owned(),
            stream_sequence: 1,
            raw_line_hash: "sha256:legacy".to_owned(),
            payload: json!({ "legacy": true }),
        })
        .payload()
        .is_none()
    );
    assert_eq!(
        DurableDomainEvent::CheckFinished(crate::event::DomainPayload {
            event_version: 1,
            payload: json!({ "event_type": DurableEventType::CheckFinished.as_str() }),
        })
        .payload()
        .and_then(|payload| payload.payload.get("event_type"))
        .and_then(|value| value.as_str()),
        Some(DurableEventType::CheckFinished.as_str())
    );
    assert_eq!(
        DurableDomainEvent::MutationArtifactCleanupRequested(crate::event::DomainPayload {
            event_version: 1,
            payload: json!({
                "event_type": DurableEventType::MutationArtifactCleanupRequested.as_str()
            }),
        })
        .event_type(),
        DurableEventType::MutationArtifactCleanupRequested
    );
}

#[test]
fn stored_event_sync_class_handles_unknown_and_non_appendable_events() {
    let unknown = StoredEvent::new_raw(
        "future_noncritical_event",
        EventClass::NonCritical,
        "event-future".to_owned(),
        "session-1".to_owned(),
        1,
        json!({}),
    )
    .expect("unknown non-critical event should build");
    assert_eq!(
        unknown
            .sync_class()
            .expect("unknown non-critical sync class should be normal"),
        EventSyncClass::NormalEvent
    );

    let critical = StoredEvent::new_raw(
        "future_critical_event",
        EventClass::Critical,
        "event-critical".to_owned(),
        "session-1".to_owned(),
        2,
        json!({}),
    )
    .expect("unknown critical event should build before reader classification");
    assert!(
        critical
            .sync_class()
            .expect_err("critical unknown events fail closed")
            .to_string()
            .contains("unknown critical event")
    );

    let legacy = StoredEvent::new_raw(
        DurableEventType::Legacy.as_str(),
        EventClass::Critical,
        "event-legacy".to_owned(),
        "session-1".to_owned(),
        3,
        json!({}),
    )
    .expect("raw legacy envelope should build for compatibility tests");
    assert!(
        legacy
            .sync_class()
            .expect_err("legacy is upcast-only")
            .to_string()
            .contains("cannot be appended")
    );
}

#[test]
fn stored_event_rejects_over_nested_payload() {
    let mut payload = json!("leaf");
    for _ in 0..MAX_PAYLOAD_DEPTH {
        payload = json!({"next": payload});
    }

    let error = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-deep".to_owned(),
        "session-1".to_owned(),
        1,
        payload,
    )
    .expect_err("over-nested event should fail");

    assert!(error.to_string().contains("nesting depth"));
}

#[test]
fn stored_event_rejects_oversized_payload() {
    let payload = json!({
        "large": "x".repeat(MAX_EVENT_BYTES)
    });

    let error = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-large".to_owned(),
        "session-1".to_owned(),
        1,
        payload,
    )
    .expect_err("oversized event should fail");

    assert!(error.to_string().contains("maximum byte size"));
}

#[test]
fn durable_event_sync_mapping_covers_all_appendable_events() {
    for event_type in ALL_DURABLE_EVENT_TYPES {
        if *event_type == DurableEventType::Legacy {
            assert!(event_type.sync_class().is_none());
            assert!(!event_type.appendable());
            continue;
        }
        assert!(
            event_type.sync_class().is_some(),
            "{} should have a sync class",
            event_type.as_str()
        );
    }

    assert_eq!(
        DurableEventType::UserMessageRecorded.sync_class(),
        Some(EventSyncClass::NormalEvent)
    );
    assert_eq!(
        DurableEventType::ToolExecutionStarted.sync_class(),
        Some(EventSyncClass::RecoveryCritical)
    );
    assert_eq!(
        DurableEventType::SandboxDecisionRecorded.sync_class(),
        Some(EventSyncClass::RecoveryCritical)
    );
    assert_eq!(
        DurableEventType::LogTailRecovered.sync_class(),
        Some(EventSyncClass::TailRecovery)
    );
}

#[test]
fn durable_event_type_expected_class_covers_all_appendable_types() {
    for event_type in ALL_DURABLE_EVENT_TYPES {
        if !event_type.appendable() {
            assert!(event_type.expected_event_class().is_none());
            continue;
        }
        assert!(
            event_type.expected_event_class().is_some(),
            "{} should have an expected event class",
            event_type.as_str()
        );
    }

    assert_eq!(
        DurableEventType::UserMessageRecorded.expected_event_class(),
        Some(EventClass::Critical)
    );
    assert_eq!(
        DurableEventType::ContextSourceCaptured.expected_event_class(),
        Some(EventClass::NonCritical)
    );
    assert_eq!(
        DurableEventType::SessionEntryRecorded.expected_event_class(),
        Some(EventClass::NonCritical)
    );
}

#[test]
fn durable_event_type_names_roundtrip_all_known_types() {
    for event_type in ALL_DURABLE_EVENT_TYPES {
        let name = event_type.as_str();
        assert_eq!(DurableEventType::from_event_type(name), Some(*event_type));
    }

    assert_eq!(DurableEventType::from_event_type("future_event"), None);
}

#[test]
fn reducer_disposition_covers_every_durable_event_variant() {
    for event_type in ALL_DURABLE_EVENT_TYPES {
        match reducer_disposition(*event_type) {
            ReducerDisposition::Consumed(reducer) => assert!(!reducer.is_empty()),
            ReducerDisposition::ExplicitlyIgnored { reducer, reason } => {
                assert!(!reducer.is_empty());
                assert!(!reason.is_empty());
            }
        }
    }
}

#[test]
fn projection_cursor_apply_decision_fails_closed_on_conflicts() {
    let applied = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        7,
        json!({"tool": "read_file"}),
    )
    .expect("event should build");
    let cursor = ProjectionCursor {
        session_id: "session-1".to_owned(),
        projection_schema_version: 1,
        last_applied_stream_sequence: 7,
        last_applied_event_id: applied.event_id.clone(),
        last_applied_record_checksum: applied.record_checksum.clone(),
    };

    assert_eq!(
        projection_apply_decision(Some(&cursor), &applied).expect("duplicate should be ignored"),
        ProjectionApplyDecision::IgnoreAlreadyApplied
    );
    assert_eq!(
        projection_apply_decision_for_record(
            Some(&cursor),
            "session-1",
            6,
            &cursor.last_applied_event_id,
            &cursor.last_applied_record_checksum,
        )
        .expect("already-applied older record with matching identity should be ignored"),
        ProjectionApplyDecision::IgnoreAlreadyApplied
    );

    let next = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-2".to_owned(),
        "session-1".to_owned(),
        8,
        json!({"tool": "read_file"}),
    )
    .expect("event should build");
    assert_eq!(
        projection_apply_decision(Some(&cursor), &next).expect("next should apply"),
        ProjectionApplyDecision::Apply
    );

    let wrong_session = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-wrong-session".to_owned(),
        "other-session".to_owned(),
        8,
        json!({"tool": "read_file"}),
    )
    .expect("event should build");
    assert!(
        projection_apply_decision(Some(&cursor), &wrong_session)
            .expect_err("session mismatch should fail")
            .to_string()
            .contains("session")
    );

    let conflict = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-conflict".to_owned(),
        "session-1".to_owned(),
        7,
        json!({"tool": "read_file"}),
    )
    .expect("event should build");
    assert!(
        projection_apply_decision(Some(&cursor), &conflict)
            .expect_err("same sequence with different event should fail")
            .to_string()
            .contains("sequence conflict")
    );

    let gap = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-gap".to_owned(),
        "session-1".to_owned(),
        9,
        json!({"tool": "read_file"}),
    )
    .expect("event should build");
    assert!(
        projection_apply_decision(Some(&cursor), &gap)
            .expect_err("gap should fail")
            .to_string()
            .contains("sequence gap")
    );

    let old = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-old".to_owned(),
        "session-1".to_owned(),
        6,
        json!({"tool": "read_file"}),
    )
    .expect("event should build");
    assert!(
        projection_apply_decision(Some(&cursor), &old)
            .expect_err("older unproven event should fail")
            .to_string()
            .contains("cannot prove")
    );
}

#[test]
fn run_event_transient_boundary_excludes_durable_facts() {
    assert!(is_transient_run_event(&RunEvent::TextDelta(
        "hello".to_owned()
    )));
    assert!(is_transient_run_event(&RunEvent::ReasoningDelta(
        "thinking".to_owned()
    )));
    assert!(is_transient_run_event(&RunEvent::ToolCallArgsDelta {
        id: "call-1".to_owned(),
        delta: "{}".to_owned(),
    }));
    assert!(!is_transient_run_event(&RunEvent::ToolCallStarted(
        tool_call("call-1")
    )));
    assert!(!is_transient_run_event(&RunEvent::ToolResult(
        ToolResult::ok("call-1", "read_file", "ok", ToolResultMeta::default())
    )));
}

#[test]
fn public_run_event_serializes_stable_text_delta_envelope() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        7,
        RunEvent::TextDelta("hello".to_owned()),
    );

    let value = serde_json::to_value(event).expect("public run event should serialize");

    assert_eq!(value["schema_version"], PUBLIC_RUN_EVENT_SCHEMA_VERSION);
    assert_eq!(value["session_id"], "session-1");
    assert_eq!(value["run_id"], "run-1");
    assert_eq!(value["sequence"], 7);
    assert_eq!(value["event"]["type"], "text_delta");
    assert_eq!(value["event"]["text"], "hello");
}

#[test]
fn public_run_event_roundtrips_tool_call_args_delta() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        8,
        RunEvent::ToolCallArgsDelta {
            id: "call-1".to_owned(),
            delta: "{\"path\"".to_owned(),
        },
    );
    let value = serde_json::to_value(&event).expect("public run event should serialize");

    let roundtripped: PublicRunEvent =
        serde_json::from_value(value.clone()).expect("public run event should deserialize");
    let roundtripped_value =
        serde_json::to_value(roundtripped).expect("public run event should serialize again");

    assert_eq!(roundtripped_value, value);
    assert_eq!(roundtripped_value["event"]["type"], "tool_call_args_delta");
    assert_eq!(roundtripped_value["event"]["id"], "call-1");
}

#[test]
fn public_run_event_projects_approval_requested_details() {
    let call = ToolCall {
        id: "call-2".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{\"path\":\"README.md\"}".to_owned(),
    };
    let spec = ToolSpec {
        name: "read_file".to_owned(),
        description: "Read a file".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string"
                }
            }
        }),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        preview: ToolPreviewCapability::None,
    };
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        9,
        RunEvent::ToolApprovalRequested {
            call,
            spec,
            subjects: vec![ToolSubject::path("README.md", "README.md")],
            operation: crate::ToolOperation::Read,
            risk: crate::PermissionRisk::Low,
            subject_zones: vec![crate::PathTrustZone::WorkspaceSource],
            confirmation: None,
            snapshot_required: false,
            preview: None,
        },
    );

    let value = serde_json::to_value(event).expect("public run event should serialize");

    assert_eq!(value["event"]["type"], "approval_requested");
    assert_eq!(value["event"]["call"]["id"], "call-2");
    assert_eq!(value["event"]["call"]["name"], "read_file");
    assert_eq!(value["event"]["spec"]["category"], "file");
    assert_eq!(value["event"]["spec"]["access"], "read");
    assert_eq!(value["event"]["subjects"][0]["kind"], "path");
    assert_eq!(value["event"]["subjects"][0]["scope"], "workspace");
    assert!(value["event"]["preview"].is_null());
}

#[test]
fn public_run_event_projects_all_internal_run_event_variants() {
    let cases = vec![
        (
            RunEvent::ReasoningDelta("thinking".to_owned()),
            "reasoning_delta",
        ),
        (
            RunEvent::ToolCallStarted(tool_call("call-start")),
            "tool_call_started",
        ),
        (
            RunEvent::ToolCallCompleted(tool_call("call-complete")),
            "tool_call_completed",
        ),
        (
            RunEvent::ToolApprovalResolved {
                call_id: "call-approval".to_owned(),
                approved: true,
                reason: Some("ok".to_owned()),
            },
            "approval_resolved",
        ),
        (
            RunEvent::ToolResult(ToolResult::ok(
                "call-result",
                "read_file",
                "done",
                ToolResultMeta::default(),
            )),
            "tool_result",
        ),
        (RunEvent::Usage(UsageStats::default()), "usage"),
        (
            RunEvent::ContinuationState(continuation_state("cursor")),
            "continuation_state",
        ),
        (RunEvent::Notice("heads up".to_owned()), "notice"),
    ];

    for (index, (event, expected_type)) in cases.into_iter().enumerate() {
        let value = serde_json::to_value(PublicRunEvent::from_run_event(
            "session-1",
            "run-1",
            index as u64,
            event,
        ))
        .expect("public run event should serialize");

        assert_eq!(value["event"]["type"], expected_type);
    }
}

#[test]
fn public_run_event_supports_adapter_lifecycle_events() {
    let started = PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::RunStarted {
            prompt: "inspect workspace".to_owned(),
        },
    );
    let cancelled = PublicRunEvent::new("session-1", "run-1", 2, PublicRunEventKind::RunCancelled);

    let started_value = serde_json::to_value(started).expect("started event should serialize");
    let cancelled_value =
        serde_json::to_value(cancelled).expect("cancelled event should serialize");

    assert_eq!(started_value["event"]["type"], "run_started");
    assert_eq!(started_value["event"]["prompt"], "inspect workspace");
    assert_eq!(cancelled_value["event"]["type"], "run_cancelled");
}

#[test]
fn public_run_event_supports_task_lifecycle_events() {
    let started = PublicRunEvent::new(
        "session-1",
        "run-1",
        3,
        PublicRunEventKind::TaskRunStarted {
            task_id: "task-1".to_owned(),
            objective: "ship public events".to_owned(),
        },
    );
    let finished = PublicRunEvent::new(
        "session-1",
        "run-1",
        4,
        PublicRunEventKind::TaskRunFinished {
            task_id: "task-1".to_owned(),
            status: "completed".to_owned(),
        },
    );

    let started_value = serde_json::to_value(started).expect("task start event should serialize");
    let finished_value =
        serde_json::to_value(finished).expect("task finish event should serialize");

    assert_eq!(started_value["event"]["type"], "task_run_started");
    assert_eq!(started_value["event"]["task_id"], "task-1");
    assert_eq!(started_value["event"]["objective"], "ship public events");
    assert_eq!(finished_value["event"]["type"], "task_run_finished");
    assert_eq!(finished_value["event"]["task_id"], "task-1");
    assert_eq!(finished_value["event"]["status"], "completed");
}

#[test]
fn public_run_event_wraps_control_entries_behind_public_boundary() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        10,
        RunEvent::Control(ControlEntry::Note {
            kind: "diagnostic".to_owned(),
            data: json!({ "value": 1 }),
        }),
    );

    let value = serde_json::to_value(event).expect("control event should serialize");

    assert_eq!(value["event"]["type"], "control");
    assert_eq!(value["event"]["control"]["kind"], "note");
    assert!(value["event"]["control"]["payload"].is_object());
}

#[test]
fn public_control_event_kinds_cover_control_entry_variants() {
    let entries = vec![
        (
            ControlEntry::SessionIdentity {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-chat".to_owned(),
            },
            "session_identity",
        ),
        (
            ControlEntry::ContinuationStateSaved(continuation_state("saved")),
            "continuation_state_saved",
        ),
        (
            ControlEntry::ResponseHandleTracked(ResponseHandle {
                provider_name: "deepseek".to_owned(),
                response_id: "response-1".to_owned(),
                continuation_cursor: None,
            }),
            "response_handle_tracked",
        ),
        (
            ControlEntry::BackgroundTaskTracked(BackgroundTaskHandle {
                provider_name: "deepseek".to_owned(),
                task_id: "remote-task-1".to_owned(),
                resumable: true,
            }),
            "background_task_tracked",
        ),
        (
            ControlEntry::PrefixSnapshotCaptured(prefix_snapshot()),
            "prefix_snapshot_captured",
        ),
        (
            ControlEntry::MemorySnapshotCaptured(MemorySnapshot {
                messages: Vec::new(),
                report: MemoryLoadReport::default(),
            }),
            "memory_snapshot_captured",
        ),
        (
            ControlEntry::UsageSnapshot(UsageStats::default()),
            "usage_snapshot",
        ),
        (
            ControlEntry::ToolApproval(ToolApprovalEntry {
                action: ToolApprovalAuditAction::Requested,
                call_id: "call-approval".to_owned(),
                tool_name: "read_file".to_owned(),
                access: ToolAccess::Read,
                subjects: Vec::new(),
                operation: None,
                risk: None,
                subject_zones: Vec::new(),
                confirmation: None,
                snapshot_required: false,
                policy_decision: ApprovalMode::Ask,
                external_directory_required: false,
                user_decision: None,
                reason: None,
                preview_hash: None,
            }),
            "tool_approval",
        ),
        (
            ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
                call_id: "call-execution".to_owned(),
                tool_name: "read_file".to_owned(),
                status: ToolExecutionStatus::Started,
                duration_ms: None,
                subjects: Vec::new(),
                changed_files: Vec::new(),
                metadata: ToolResultMeta::default(),
                error: None,
                model_content_hash: None,
            })),
            "tool_execution",
        ),
        (
            ControlEntry::ToolEgress(Box::new(ToolEgressEntry {
                call_id: "call-egress".to_owned(),
                tool_name: "mcp__server__tool".to_owned(),
                destination: "server".to_owned(),
                operation: "call_tool".to_owned(),
                subjects: Vec::new(),
                payload: json!({ "redacted": true }),
                redacted: true,
            })),
            "tool_egress",
        ),
        (
            ControlEntry::McpElicitation(Box::new(McpElicitationEntry::new(
                "server",
                "continue?",
                &json!({ "type": "object" }),
                McpElicitationDecision::Declined,
                None,
            ))),
            "mcp_elicitation",
        ),
        (
            ControlEntry::ToolPreviewCaptured(tool_preview_snapshot()),
            "tool_preview_captured",
        ),
        (
            ControlEntry::SkillIndexCaptured(
                SkillIndexSnapshot::new(vec![skill_descriptor()])
                    .expect("valid skill index snapshot"),
            ),
            "skill_index_captured",
        ),
        (
            ControlEntry::SkillLoaded(SkillLoadEntry {
                skill_id: "repo-review".to_owned(),
                sha256: "hash".to_owned(),
                source: SkillSource::Workspace,
                entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
                run_id: Some("run-1".to_owned()),
                call_id: Some("call-1".to_owned()),
                byte_count: 128,
                line_count: 7,
                loaded_at_ms: 42,
            }),
            "skill_loaded",
        ),
        (
            ControlEntry::PluginManifestCaptured(PluginManifestSnapshot {
                plugin_id: "repo-review".to_owned(),
                name: "Repository Review".to_owned(),
                version: "0.1.0".to_owned(),
                description: None,
                manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
                manifest_hash: "sha256:manifest".to_owned(),
                capabilities: vec![PluginCapability::Skill {
                    path: "skills/review/SKILL.md".into(),
                }],
                trust: PluginTrustDecision::NeedsReview,
            }),
            "plugin_manifest_captured",
        ),
        (
            ControlEntry::PluginTrustDecision(PluginTrustEntry {
                plugin_id: "repo-review".to_owned(),
                manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
                manifest_hash: "sha256:manifest".to_owned(),
                manifest_version: None,
                capability_digest: None,
                decision: PluginTrustDecision::Trusted,
                reviewed_at_ms: 42,
            }),
            "plugin_trust_decision",
        ),
        (
            ControlEntry::ChangeSetProposed(ChangeSet {
                id: ChangeSetId::new("change-1").expect("valid change set id"),
                title: "Update README".to_owned(),
                summary: "Update project overview".to_owned(),
                risk: ChangeSetRisk::Low,
                files: Vec::new(),
                validations: Vec::new(),
            }),
            "change_set_proposed",
        ),
        (
            ControlEntry::ChangeSetApplied(ChangeSetResult {
                id: ChangeSetId::new("change-1").expect("valid change set id"),
                status: ChangeSetResultStatus::Applied,
                file_results: Vec::new(),
                message: None,
            }),
            "change_set_applied",
        ),
        (
            ControlEntry::TerminalTask(TerminalTaskEntry {
                handle: TerminalTaskHandle {
                    task_id: TerminalTaskId::new("terminal-1").expect("valid terminal task id"),
                    command: "cargo test".to_owned(),
                    cwd: ".".into(),
                    shell: "zsh".to_owned(),
                    log_path: ".sigil/terminal/terminal-1/output.log".into(),
                    created_at_ms: 100,
                    execution_backend: None,
                    execution_backend_capabilities: None,
                },
                status: TerminalTaskStatus::Running,
                output_preview: Some("running".to_owned()),
                output_hash: Some("sha256:abc".to_owned()),
                output_truncated: false,
                updated_at_ms: 120,
            }),
            "terminal_task",
        ),
        (
            ControlEntry::CompactionApplied(CompactionRecord {
                summary: "summary".to_owned(),
                compacted_message_count: 2,
                retained_tail_message_count: 1,
                task_memory: None,
            }),
            "compaction_applied",
        ),
        (
            ControlEntry::PlanApproved(PlanApprovedEntry {
                plan_version: 1,
                plan_hash: "sha256:plan".to_owned(),
                approved_at_ms: 42,
                permission: PlanApprovalPermission::WorkspaceEdits,
                scope: PlanApprovalScope {
                    summary: "workspace edits".to_owned(),
                    workspace_paths: vec!["crates/sigil-kernel".into()],
                },
                expires: PlanApprovalExpiry::NextUserPrompt,
                clear_planning_context: true,
            }),
            "plan_approved",
        ),
        (ControlEntry::TaskRun(task_run_entry()), "task_run"),
        (
            ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: task_id(),
                plan_version: 1,
                status: TaskPlanStatus::Accepted,
                steps: Vec::new(),
                reason: None,
            }),
            "task_plan",
        ),
        (
            ControlEntry::TaskStep(TaskStepEntry {
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                role: AgentRole::Executor,
                status: TaskStepStatus::Running,
                title: Some("implement".to_owned()),
                summary: None,
                reason: None,
            }),
            "task_step",
        ),
        (
            ControlEntry::TaskChildSession(TaskChildSessionEntry {
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                child_task_id: TaskId::new("child-task").expect("valid task id"),
                child_session_ref: session_ref(),
                role: AgentRole::SubagentRead,
                status: TaskChildSessionStatus::Started,
                summary_hash: None,
            }),
            "task_child_session",
        ),
        (
            ControlEntry::TaskChildSessionDisplayName(TaskChildSessionDisplayNameEntry {
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                child_task_id: TaskId::new("child-task").expect("valid task id"),
                display_name: "repo audit".to_owned(),
            }),
            "task_child_session_display_name",
        ),
        (
            ControlEntry::TaskSubagentApprovalRoute(TaskSubagentApprovalRouteEntry {
                route_id: route_id(),
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                role: AgentRole::SubagentWrite,
                child_session_ref: session_ref(),
                call_id: "call-child".to_owned(),
                tool_name: "write_file".to_owned(),
                status: TaskRouteStatus::Registered,
            }),
            "task_subagent_approval_route",
        ),
        (
            ControlEntry::TaskSubagentElicitationRoute(TaskSubagentElicitationRouteEntry {
                route_id: route_id(),
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                role: AgentRole::SubagentRead,
                child_session_ref: session_ref(),
                server_name: "server".to_owned(),
                status: TaskRouteStatus::Requested,
            }),
            "task_subagent_elicitation_route",
        ),
        (
            ControlEntry::CheckSpecRecorded(event_check_spec_recorded_entry()),
            "check_spec_recorded",
        ),
        (
            ControlEntry::VerificationPolicyChanged(
                event_verification_policy_changed_entry()
                    .expect("sample verification policy should hash"),
            ),
            "verification_policy_changed",
        ),
        (
            ControlEntry::VerificationCheckRun(event_verification_check_run_entry()),
            "verification_check_run",
        ),
        (
            ControlEntry::VerificationRecorded(event_verification_recorded_entry()),
            "verification_recorded",
        ),
        (
            ControlEntry::ReadinessEvaluated(event_readiness_evaluated_entry()),
            "readiness_evaluated",
        ),
        (
            ControlEntry::ChildVerificationReceiptLinked(event_child_verification_receipt_linked()),
            "child_verification_receipt_linked",
        ),
        (
            ControlEntry::WorkspaceTrustDecision(event_workspace_trust_decision_entry()),
            "workspace_trust_decision",
        ),
        (
            ControlEntry::AgentProfileCaptured(AgentProfileCapturedEntry {
                snapshot: agent_profile_snapshot(),
            }),
            "agent_profile_captured",
        ),
        (
            ControlEntry::AgentProfileTrustDecision(AgentProfileTrustEntry {
                profile_id: agent_profile_id(),
                source: AgentProfileSource::Workspace,
                source_hash: "sha256:source".to_owned(),
                profile_hash: "sha256:profile".to_owned(),
                decision: AgentTrustState::Trusted,
                reviewed_at_ms: 42,
            }),
            "agent_profile_trust_decision",
        ),
        (
            ControlEntry::AgentProfilePolicyDecision(AgentProfilePolicyEntry {
                profile_id: agent_profile_id(),
                source: AgentProfileSource::Workspace,
                source_hash: "sha256:source".to_owned(),
                profile_hash: "sha256:profile".to_owned(),
                enabled: Some(true),
                user_invocable: Some(true),
                model_invocable: Some(false),
                reviewed_at_ms: 42,
            }),
            "agent_profile_policy_decision",
        ),
        (
            ControlEntry::AgentThreadStarted(AgentThreadStartedEntry {
                thread_id: agent_thread_id(),
                parent_thread_id: Some(AgentThreadId::new("main").expect("valid thread id")),
                parent_session_ref: session_ref(),
                thread_session_ref: agent_session_ref(),
                profile_id: agent_profile_id(),
                profile_snapshot_id: agent_snapshot_id(),
                run_context: agent_run_context(),
                objective: "inspect kernel".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: AgentInvocationMode::Foreground,
                invocation_source: AgentInvocationSource::Chat,
                display_name: Some("kernel map".to_owned()),
                created_at_ms: Some(42),
            }),
            "agent_thread_started",
        ),
        (
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: agent_thread_id(),
                status: AgentThreadStatus::Running,
                reason: None,
                updated_at_ms: Some(43),
            }),
            "agent_thread_status_changed",
        ),
        (
            ControlEntry::AgentThreadMessageRouted(AgentThreadMessageRoutedEntry {
                route_id: agent_route_id(),
                source_thread_id: AgentThreadId::new("main").expect("valid thread id"),
                target_thread_id: agent_thread_id(),
                prompt_hash: "sha256:steer".to_owned(),
                prompt: None,
                status: AgentRouteStatus::Resolved,
            }),
            "agent_thread_message_routed",
        ),
        (
            ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry {
                result: AgentThreadResult {
                    thread_id: agent_thread_id(),
                    session_ref: agent_session_ref(),
                    status: AgentThreadTerminalStatus::Completed,
                    summary: "done".to_owned(),
                    summary_truncated: false,
                    original_summary_chars: None,
                    artifacts: Vec::new(),
                    changed_paths: Vec::new(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage: None,
                    output_hash: "sha256:result".to_owned(),
                    final_answer_ref: None,
                },
            }),
            "agent_thread_result_recorded",
        ),
        (
            ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
                thread_id: agent_thread_id(),
                status: AgentResultContinuationStatus::Pending,
                reason: Some("child result ready".to_owned()),
                updated_at_ms: Some(44),
            }),
            "agent_result_continuation",
        ),
        (
            ControlEntry::AgentThreadDisplayName(AgentThreadDisplayNameEntry {
                thread_id: agent_thread_id(),
                display_name: "kernel map".to_owned(),
            }),
            "agent_thread_display_name",
        ),
        (
            ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: AgentRouteId::new("agent-route-approval").expect("valid route id"),
                source_thread_id: agent_thread_id(),
                target_thread_id: Some(AgentThreadId::new("main").expect("valid thread id")),
                call_id: "call-agent".to_owned(),
                tool_name: "read_file".to_owned(),
                status: AgentRouteStatus::Requested,
            }),
            "agent_approval_route",
        ),
        (
            ControlEntry::AgentElicitationRoute(AgentElicitationRouteEntry {
                route_id: AgentRouteId::new("agent-route-elicitation").expect("valid route id"),
                source_thread_id: agent_thread_id(),
                target_thread_id: Some(AgentThreadId::new("main").expect("valid thread id")),
                server_name: "filesystem".to_owned(),
                status: AgentRouteStatus::Registered,
            }),
            "agent_elicitation_route",
        ),
        (
            ControlEntry::AgentRunAttemptStarted(AgentRunAttemptStartedEntry {
                thread_id: agent_thread_id(),
                attempt_id: agent_attempt_id(),
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                background: true,
                provider_background_handle_ref: Some("opaque-handle".to_owned()),
            }),
            "agent_run_attempt_started",
        ),
        (
            ControlEntry::AgentRunHeartbeat(AgentRunHeartbeatEntry {
                thread_id: agent_thread_id(),
                attempt_id: agent_attempt_id(),
                updated_at_ms: 44,
            }),
            "agent_run_heartbeat",
        ),
        (
            ControlEntry::AgentRunInterrupted(AgentRunInterruptedEntry {
                thread_id: agent_thread_id(),
                attempt_id: agent_attempt_id(),
                reason: "restore".to_owned(),
            }),
            "agent_run_interrupted",
        ),
        (
            ControlEntry::AgentRouteClosed(AgentRouteClosedEntry {
                route_id: agent_route_id(),
                reason: "restore".to_owned(),
            }),
            "agent_route_closed",
        ),
        (
            ControlEntry::AgentMergeSafePoint(AgentMergeSafePointEntry {
                thread_id: agent_thread_id(),
                parent_thread_id: AgentThreadId::new("main").expect("valid thread id"),
                result_hash: "sha256:result".to_owned(),
            }),
            "agent_merge_safe_point",
        ),
        (
            ControlEntry::AgentThreadClosed(AgentThreadClosedEntry {
                thread_id: agent_thread_id(),
                reason: Some("archived".to_owned()),
            }),
            "agent_thread_closed",
        ),
        (
            ControlEntry::ConversationInputQueued(ConversationInputQueuedEntry {
                queue_id: conversation_queue_id(),
                target: ConversationInputTarget::MainThread,
                kind: ConversationInputKind::Chat,
                prompt_hash: "sha256:queued".to_owned(),
                prompt: "follow up".to_owned(),
                reasoning_effort: Some(ReasoningEffort::Max),
                created_at_ms: Some(45),
            }),
            "conversation_input_queued",
        ),
        (
            ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
                action: ConversationInputQueueControlAction::Pause,
                reason: Some("user paused".to_owned()),
                updated_at_ms: Some(46),
            }),
            "conversation_input_queue_control",
        ),
        (
            ControlEntry::ConversationInputEdited(ConversationInputEditedEntry {
                queue_id: conversation_queue_id(),
                prompt_hash: "sha256:edited".to_owned(),
                prompt: "edited follow up".to_owned(),
                reasoning_effort: None,
                updated_at_ms: Some(47),
            }),
            "conversation_input_edited",
        ),
        (
            ControlEntry::ConversationInputReordered(ConversationInputReorderedEntry {
                queue_id: conversation_queue_id(),
                after_queue_id: None,
                updated_at_ms: Some(48),
            }),
            "conversation_input_reordered",
        ),
        (
            ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
                queue_id: conversation_queue_id(),
                status: ConversationInputStatus::Delivered,
                reason: Some("sent now".to_owned()),
                updated_at_ms: Some(49),
            }),
            "conversation_input_status_changed",
        ),
        (
            ControlEntry::Note {
                kind: "diagnostic".to_owned(),
                data: json!({ "value": 1 }),
            },
            "note",
        ),
    ];

    for (entry, expected_kind) in entries {
        let control = PublicControlEvent::from(entry);

        assert_eq!(control.kind, expected_kind);
        assert!(control.payload.is_some());
    }
}

#[test]
fn public_run_event_projects_assistant_message_to_public_dto() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        11,
        RunEvent::AssistantMessage(ModelMessage::assistant(
            Some("done".to_owned()),
            vec![ToolCall {
                id: "call-3".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{}".to_owned(),
            }],
        )),
    );

    let value = serde_json::to_value(event).expect("assistant event should serialize");

    assert_eq!(value["event"]["type"], "assistant_message");
    assert_eq!(value["event"]["message"]["content"], "done");
    assert_eq!(value["event"]["message"]["tool_calls"][0]["id"], "call-3");
    assert!(value["event"]["message"]["role"].is_null());
    assert!(value["event"]["message"]["tool_call_id"].is_null());
}

fn tool_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "read_file".to_owned(),
        args_json: "{\"path\":\"README.md\"}".to_owned(),
    }
}

fn continuation_state(state_kind: &str) -> ProviderContinuationState {
    ProviderContinuationState {
        provider_name: "deepseek".to_owned(),
        state_kind: state_kind.to_owned(),
        message_id: None,
        opaque_blob: json!({ "cursor": "cursor-1" }),
    }
}

fn prefix_snapshot() -> PrefixSnapshot {
    PrefixSnapshot {
        materialized_text: "system".to_owned(),
        sha256: "hash".to_owned(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-chat".to_owned(),
        memory_fingerprint: "memory".to_owned(),
        tool_schema_fingerprint: "tools".to_owned(),
        skill_index_fingerprint: "skills".to_owned(),
    }
}

fn skill_descriptor() -> SkillDescriptor {
    SkillDescriptor {
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
    }
}

fn tool_preview_snapshot() -> ToolPreviewSnapshot {
    ToolPreviewSnapshot::from_preview(
        "call-preview",
        "write_file",
        &ToolPreview {
            title: "Write file".to_owned(),
            summary: "Create file".to_owned(),
            body: "preview".to_owned(),
            changed_files: vec!["README.md".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "README.md".to_owned(),
                diff: "--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+hello".to_owned(),
            }],
        },
        Default::default(),
        Some("preview-hash".to_owned()),
    )
}

fn event_check_spec() -> CheckSpec {
    CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        "scope-main",
    )
}

fn stored_control_event(
    event_type: DurableEventType,
    control: ControlEntry,
    stream_sequence: u64,
) -> StoredEvent {
    StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("known durable event type should have expected class"),
        format!("event-{}", stream_sequence),
        "session-1".to_owned(),
        stream_sequence,
        json!({ "session_log_entry": SessionLogEntry::Control(control) }),
    )
    .expect("control event should build")
}

fn sample_terminal_task_entry() -> TerminalTaskEntry {
    TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id: TerminalTaskId::new("terminal-1").expect("valid terminal task id"),
            command: "cargo test".to_owned(),
            cwd: ".".into(),
            shell: "zsh".to_owned(),
            log_path: ".sigil/terminal/terminal-1/output.log".into(),
            created_at_ms: 100,
            execution_backend: None,
            execution_backend_capabilities: None,
        },
        status: TerminalTaskStatus::Running,
        output_preview: Some("running".to_owned()),
        output_hash: Some("sha256:abc".to_owned()),
        output_truncated: false,
        updated_at_ms: 120,
    }
}

fn sample_change_set() -> ChangeSet {
    ChangeSet {
        id: ChangeSetId::new("change-1").expect("valid change set id"),
        title: "Update README".to_owned(),
        summary: "Update project overview".to_owned(),
        risk: ChangeSetRisk::Low,
        files: Vec::new(),
        validations: Vec::new(),
    }
}

fn event_verification_policy() -> VerificationPolicy {
    VerificationPolicy {
        required_checks: vec![event_check_spec()],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked("scope-main"),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: None,
        auto_run: VerificationAutoRunPolicy::Manual,
    }
}

fn event_check_spec_recorded_entry() -> CheckSpecRecordedEntry {
    let candidate = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    };
    let trusted = candidate
        .promote(
            "cargo-test",
            "scope-main",
            ToolEffect::ReadOnly,
            CheckPromotion::ExplicitUserConfig {
                config_event_id: "event-config".to_owned(),
            },
        )
        .expect("sample check should promote");
    CheckSpecRecordedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        trusted,
        "event-config",
    )
}

fn event_verification_policy_changed_entry() -> anyhow::Result<VerificationPolicyChangedEntry> {
    VerificationPolicyChangedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        event_verification_policy(),
        "event-policy",
    )
}

fn event_verification_recorded_entry() -> VerificationRecordedEntry {
    let check = event_check_spec();
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

fn event_verification_check_run_entry() -> VerificationCheckRunEntry {
    let check = event_check_spec();
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

fn event_readiness_evaluated_entry() -> ReadinessEvaluatedEntry {
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

fn event_child_verification_receipt_linked() -> ChildVerificationReceiptLinked {
    ChildVerificationReceiptLinked {
        parent_session_id: "parent-session".to_owned(),
        child_session_id: "child-session".to_owned(),
        child_receipt_id: "child-receipt".to_owned(),
        child_event_id: "child-event".to_owned(),
        child_workspace_id: "child-workspace".to_owned(),
        child_workspace_snapshot_id: "child-snapshot".to_owned(),
        policy_hash: "policy-hash".to_owned(),
        changeset_id: None,
        merge_event_id: None,
    }
}

fn event_workspace_trust_decision_entry() -> WorkspaceTrustDecisionEntry {
    WorkspaceTrustDecisionEntry {
        workspace_id: "workspace-1".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
        trust: WorkspaceTrust::Trusted,
        decided_by_event_id: Some("event-trust".to_owned()),
        reason: Some("user trusted workspace".to_owned()),
    }
}

fn task_id() -> TaskId {
    TaskId::new("task-1").expect("valid task id")
}

fn step_id() -> TaskStepId {
    TaskStepId::new("step-1").expect("valid step id")
}

fn route_id() -> TaskRouteId {
    TaskRouteId::new("route-1").expect("valid route id")
}

fn session_ref() -> SessionRef {
    SessionRef::new_relative("child.jsonl").expect("valid session ref")
}

fn agent_session_ref() -> SessionRef {
    SessionRef::new_relative("children/agent-thread-1.jsonl").expect("valid session ref")
}

fn agent_profile_id() -> AgentProfileId {
    AgentProfileId::new("explore").expect("valid profile id")
}

fn agent_snapshot_id() -> AgentProfileSnapshotId {
    AgentProfileSnapshotId::new("snapshot-1").expect("valid snapshot id")
}

fn agent_thread_id() -> AgentThreadId {
    AgentThreadId::new("agent-thread-1").expect("valid thread id")
}

fn agent_attempt_id() -> AgentRunAttemptId {
    AgentRunAttemptId::new("attempt-1").expect("valid attempt id")
}

fn agent_route_id() -> AgentRouteId {
    AgentRouteId::new("agent-route-1").expect("valid route id")
}

fn conversation_queue_id() -> ConversationInputQueueId {
    ConversationInputQueueId::new("queue_1").expect("valid queue id")
}

fn agent_profile_snapshot() -> AgentProfileSnapshot {
    AgentProfileSnapshot {
        snapshot_id: agent_snapshot_id(),
        profile_id: agent_profile_id(),
        source: AgentProfileSource::Workspace,
        source_hash: "sha256:source".to_owned(),
        profile_hash: "sha256:profile".to_owned(),
        resolved_tool_scope_hash: "sha256:tools".to_owned(),
        resolved_permission_policy_hash: "sha256:permissions".to_owned(),
        resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
        resolved_skill_hashes: Vec::new(),
        trust_state: AgentTrustState::Trusted,
    }
}

fn agent_run_context() -> AgentRunContextSnapshot {
    AgentRunContextSnapshot {
        profile_snapshot_id: agent_snapshot_id(),
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        reasoning_effort: None,
        workspace_root: WorkspaceRootSnapshot::new("/workspace").expect("valid workspace root"),
        effective_tool_scope_hash: "sha256:tools".to_owned(),
        effective_permission_policy_hash: "sha256:permissions".to_owned(),
        effective_mcp_scope_hash: "sha256:mcp".to_owned(),
        provider_capability_hash: "sha256:provider".to_owned(),
        model_visible_agent_index_hash: None,
        budget_policy_hash: "sha256:budget".to_owned(),
        provider_background_handle_ref: None,
    }
}

fn task_run_entry() -> TaskRunEntry {
    TaskRunEntry {
        task_id: task_id(),
        parent_session_ref: session_ref(),
        objective: "implement public events".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }
}
