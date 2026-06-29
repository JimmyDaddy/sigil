use anyhow::Result;
use serde_json::json;

use crate::{
    ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus, ChangeSetId,
    ChangeSetResult, ChangeSetResultStatus, CompactionRecord, ControlEntry, DurableEventType,
    EventClass, EvidenceReceipt, EvidenceScope, FileChangeRef, FileType, LegacyEvent,
    ModelAssistedMemoryDecision, ModelAssistedMemoryFact, ModelAssistedTaskMemorySummary,
    MutationCommitted, MutationSubject, ReceiptStatus, RedactionState, SessionLogEntry,
    SessionStreamRecord, SourcedDecision, SourcedFact, StoredEvent, TaskMemoryExtractionInput,
    TaskMemoryV1, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepStatus, ToolExecutionEntry,
    ToolExecutionStatus, ToolResultMeta, VerificationBinding, VerificationReceipt,
    VerificationRecordedEntry, extract_task_memory_from_stream_records, task_memory_context_items,
};

fn sample_task_memory() -> TaskMemoryV1 {
    TaskMemoryV1 {
        memory_id: "mem-1".to_owned(),
        branch_id: Some("main".to_owned()),
        valid_for_snapshot: "snapshot-1".to_owned(),
        supersedes: None,
        source_event_ids: vec!["event-1".to_owned()],
        objective: "Implement plugin trust hardening".to_owned(),
        constraints: vec![SourcedFact::system_derived(
            "Do not run plugin code before trust",
            "event-1",
        )],
        decisions: vec![SourcedDecision {
            decision: SourcedFact::system_derived("Bind trust to digest and version", "event-2"),
            rationale: Some(SourcedFact::model_inferred(
                "Digest-only trust is harder to inspect in product surfaces",
                "event-3",
            )),
        }],
        files_changed: vec![FileChangeRef {
            path: "crates/sigil-kernel/src/plugin.rs".into(),
            source_event_id: Some("event-4".to_owned()),
            mutation_receipt_id: Some("mutation-1".to_owned()),
        }],
        commands_run: vec!["cmd-1".to_owned()],
        verification_results: vec!["check-1".to_owned()],
        failed_attempts: Vec::new(),
        risks: vec![SourcedFact::model_inferred(
            "Legacy trust entries may not contain capability digests",
            "event-5",
        )],
        unresolved_issues: vec![SourcedFact::system_derived(
            "Extension execution isolation remains a later slice",
            "event-6",
        )],
    }
}

#[test]
fn compaction_memory_task_memory_v1_roundtrips_with_sources() -> Result<()> {
    let memory = sample_task_memory();

    memory.validate()?;
    let json = serde_json::to_string(&memory)?;
    let restored: TaskMemoryV1 = serde_json::from_str(&json)?;

    assert_eq!(restored, memory);
    assert_eq!(
        restored.files_changed[0].mutation_receipt_id.as_deref(),
        Some("mutation-1")
    );
    assert_eq!(restored.verification_results, vec!["check-1"]);
    Ok(())
}

#[test]
fn compaction_memory_record_preserves_legacy_summary_fallback() -> Result<()> {
    let legacy_json = r#"{
        "summary": "legacy local summary",
        "compacted_message_count": 3,
        "retained_tail_message_count": 2
    }"#;

    let restored: CompactionRecord = serde_json::from_str(legacy_json)?;

    assert_eq!(restored.summary, "legacy local summary");
    assert_eq!(restored.compacted_message_count, 3);
    assert_eq!(restored.retained_tail_message_count, 2);
    assert!(restored.task_memory.is_none());
    Ok(())
}

#[test]
fn compaction_memory_record_can_attach_typed_task_memory() -> Result<()> {
    let record = CompactionRecord {
        summary: "legacy local summary".to_owned(),
        compacted_message_count: 4,
        retained_tail_message_count: 2,
        task_memory: Some(sample_task_memory()),
    };

    record
        .task_memory
        .as_ref()
        .expect("task memory should exist")
        .validate()?;
    let json = serde_json::to_string(&record)?;
    let restored: CompactionRecord = serde_json::from_str(&json)?;

    assert_eq!(restored, record);
    assert!(json.contains("task_memory"));
    Ok(())
}

#[test]
fn compaction_memory_model_generated_fact_cannot_create_verified_evidence() {
    let mut fact = SourcedFact::model_inferred("tests passed", "event-model-summary");
    fact.verified = true;

    let error = fact
        .validate()
        .expect_err("model summary should not create verified evidence");

    assert!(
        error
            .to_string()
            .contains("cannot be verified without durable evidence")
    );
}

#[test]
fn compaction_model_summary_marks_imported_facts_unverified() -> Result<()> {
    let mut memory = sample_task_memory();

    memory.merge_model_summary(ModelAssistedTaskMemorySummary {
        source_event_id: "event-model-summary".to_owned(),
        constraints: vec![ModelAssistedMemoryFact {
            text: "Keep plugin trust decisions inspectable".to_owned(),
            confidence_percent: Some(75),
        }],
        decisions: vec![ModelAssistedMemoryDecision {
            decision: ModelAssistedMemoryFact {
                text: "Defer plugin process execution".to_owned(),
                confidence_percent: Some(80),
            },
            rationale: Some(ModelAssistedMemoryFact {
                text: "No durable plugin-owned process runtime exists yet".to_owned(),
                confidence_percent: Some(70),
            }),
        }],
        risks: vec![ModelAssistedMemoryFact {
            text: "Model summary may miss an unresolved edge".to_owned(),
            confidence_percent: Some(60),
        }],
        unresolved_issues: vec![ModelAssistedMemoryFact {
            text: "Hook output still needs egress policy checks".to_owned(),
            confidence_percent: None,
        }],
    })?;

    let imported_constraint = memory
        .constraints
        .iter()
        .find(|fact| fact.text == "Keep plugin trust decisions inspectable")
        .expect("model imported constraint should exist");
    assert!(imported_constraint.model_generated);
    assert!(!imported_constraint.verified);
    assert_eq!(imported_constraint.confidence_percent, Some(75));
    assert_eq!(
        imported_constraint.source_event_id.as_deref(),
        Some("event-model-summary")
    );
    assert!(memory.decisions.iter().any(|decision| {
        decision.decision.model_generated
            && !decision.decision.verified
            && decision
                .rationale
                .as_ref()
                .is_some_and(|rationale| rationale.model_generated && !rationale.verified)
    }));
    Ok(())
}

#[test]
fn compaction_model_summary_rejects_invalid_confidence() {
    let mut memory = sample_task_memory();

    let error = memory
        .merge_model_summary(ModelAssistedTaskMemorySummary {
            source_event_id: "event-model-summary".to_owned(),
            constraints: vec![ModelAssistedMemoryFact {
                text: "too confident".to_owned(),
                confidence_percent: Some(101),
            }],
            decisions: Vec::new(),
            risks: Vec::new(),
            unresolved_issues: Vec::new(),
        })
        .expect_err("invalid model confidence should fail");

    assert!(error.to_string().contains("confidence must be 0..=100"));
}

#[test]
fn compaction_memory_context_items_preserve_task_memory_provenance() -> Result<()> {
    let memory = sample_task_memory();

    let items = task_memory_context_items(&memory)?;

    assert!(items.iter().any(|item| {
        item.id == "task-memory:mem-1:objective"
            && item.source == crate::ContextSource::TaskDigest
            && item.trust_level == crate::ContextTrustLevel::ToolObservation
            && item.sensitivity == crate::ContextSensitivity::Repository
            && item.source_event_id.as_deref() == Some("event-1")
    }));
    assert!(items.iter().any(|item| {
        item.id == "task-memory:mem-1:decision:0"
            && item.source_event_id.as_deref() == Some("event-2")
    }));
    assert!(items.iter().any(|item| {
        item.id == "task-memory:mem-1:file:0" && item.source_event_id.as_deref() == Some("event-4")
    }));
    Ok(())
}

#[test]
fn compaction_memory_validation_rejects_malformed_edges() {
    let mut empty_fact = SourcedFact::system_derived("   ", "event-empty");
    assert!(empty_fact.validate().is_err());

    empty_fact.text = "high confidence".to_owned();
    empty_fact.confidence_percent = Some(101);
    assert!(empty_fact.validate().is_err());

    assert!(FileChangeRef::new("src/lib.rs").validate().is_ok());
    assert!(FileChangeRef::new("").validate().is_err());

    assert!(
        crate::AttemptRef {
            attempt_id: " ".to_owned(),
            source_event_id: None,
            summary: None,
        }
        .validate()
        .is_err()
    );

    let mut memory = sample_task_memory();
    memory.memory_id = String::new();
    assert!(memory.validate().is_err());
    memory.memory_id = "mem-1".to_owned();
    memory.valid_for_snapshot = String::new();
    assert!(memory.validate().is_err());
    memory.valid_for_snapshot = "snapshot-1".to_owned();
    memory.objective = String::new();
    assert!(memory.validate().is_err());
}

#[test]
fn compaction_extraction_builds_task_memory_from_structured_durable_events() -> Result<()> {
    let task = SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: crate::TaskId::new("task-1")?,
        parent_session_ref: crate::SessionRef::new_relative("session-parent.jsonl")?,
        objective: "Fix plugin trust projection".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }));
    let tool =
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-bash".to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(10),
            subjects: Vec::new(),
            changed_files: vec!["crates/sigil-kernel/src/plugin.rs".to_owned()],
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        })));
    let verification = SessionLogEntry::Control(ControlEntry::VerificationRecorded(
        VerificationRecordedEntry {
            receipt: verification_receipt("check-1", "event-verification"),
        },
    ));
    let mutation = MutationCommitted {
        operation_id: "op-1".to_owned(),
        batch_id: None,
        workspace_id: Some("workspace-1".to_owned()),
        observed_after_hash: Some("sha256:file".to_owned()),
        workspace_revision: 3,
        workspace_snapshot_id: "snapshot-2".to_owned(),
        committed_subject: MutationSubject::File {
            path: "README.md".into(),
            file_type: FileType::File,
        },
    };
    let records = vec![
        stored_session_entry(1, "event-task", task)?,
        stored_session_entry(2, "event-tool", tool)?,
        stored_session_entry(3, "event-verification", verification)?,
        stored_mutation_committed(4, "event-mutation", mutation)?,
    ];

    let memory = extract_task_memory_from_stream_records(
        &records,
        TaskMemoryExtractionInput {
            memory_id: "memory-1".to_owned(),
            valid_for_snapshot: "snapshot-2".to_owned(),
            branch_id: Some("main".to_owned()),
            supersedes: None,
            objective: None,
        },
    )?;

    assert_eq!(memory.objective, "Fix plugin trust projection");
    assert_eq!(
        memory.source_event_ids,
        vec![
            "event-task",
            "event-tool",
            "event-verification",
            "event-mutation"
        ]
    );
    assert_eq!(memory.commands_run, vec!["call-bash"]);
    assert_eq!(memory.verification_results, vec!["check-1"]);
    assert!(memory.files_changed.iter().any(|file| {
        file.path == std::path::Path::new("crates/sigil-kernel/src/plugin.rs")
            && file.source_event_id.as_deref() == Some("event-tool")
    }));
    assert!(memory.files_changed.iter().any(|file| {
        file.path == std::path::Path::new("README.md")
            && file.mutation_receipt_id.as_deref() == Some("op-1")
    }));
    Ok(())
}

#[test]
fn compaction_extraction_keeps_failed_steps_as_attempt_refs_without_verification_claims()
-> Result<()> {
    let failed_tool =
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-test".to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Failed,
            duration_ms: Some(20),
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: Some(crate::ToolError {
                kind: crate::ToolErrorKind::ExitStatus,
                message: "tests failed".to_owned(),
                retryable: false,
                details: json!({}),
            }),
            model_content_hash: None,
        })));

    let memory = extract_task_memory_from_stream_records(
        &[stored_session_entry(1, "event-failed-tool", failed_tool)?],
        TaskMemoryExtractionInput {
            memory_id: "memory-2".to_owned(),
            valid_for_snapshot: "snapshot-3".to_owned(),
            branch_id: None,
            supersedes: Some("memory-1".to_owned()),
            objective: Some("Run tests".to_owned()),
        },
    )?;

    assert_eq!(memory.supersedes.as_deref(), Some("memory-1"));
    assert!(memory.verification_results.is_empty());
    assert_eq!(memory.failed_attempts.len(), 1);
    assert_eq!(memory.failed_attempts[0].attempt_id, "call-test");
    assert_eq!(
        memory.failed_attempts[0].summary.as_deref(),
        Some("tests failed")
    );
    Ok(())
}

#[test]
fn compaction_extraction_handles_legacy_failed_task_blocked_step_and_changeset() -> Result<()> {
    let failed_task = SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: crate::TaskId::new("task-legacy")?,
        parent_session_ref: crate::SessionRef::new_relative("session-parent.jsonl")?,
        objective: "Recover failed task".to_owned(),
        status: TaskRunStatus::Failed,
        reason: Some("planner failed".to_owned()),
    }));
    let blocked_step = SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: crate::TaskId::new("task-legacy")?,
        plan_version: 1,
        step_id: crate::TaskStepId::new("verify")?,
        role: crate::AgentRole::Executor,
        status: TaskStepStatus::Blocked,
        title: Some("Verify".to_owned()),
        summary: None,
        reason: Some("missing check".to_owned()),
    }));
    let changeset = SessionLogEntry::Control(ControlEntry::ChangeSetApplied(ChangeSetResult {
        id: ChangeSetId::new("changeset-1")?,
        status: ChangeSetResultStatus::Failed,
        file_results: vec![ChangeSetFileResult {
            path: "src/lib.rs".to_owned(),
            action: ChangeSetFileAction::Update,
            status: ChangeSetFileResultStatus::Failed,
            message: Some("conflict".to_owned()),
            validations: Vec::new(),
        }],
        message: Some("changeset failed".to_owned()),
    }));
    let records = vec![
        legacy_session_entry(1, "event-legacy-task", failed_task),
        stored_session_entry(2, "event-blocked-step", blocked_step)?,
        stored_session_entry(3, "event-changeset", changeset)?,
    ];

    let memory = extract_task_memory_from_stream_records(
        &records,
        TaskMemoryExtractionInput {
            memory_id: "memory-legacy".to_owned(),
            valid_for_snapshot: "snapshot-legacy".to_owned(),
            branch_id: None,
            supersedes: None,
            objective: None,
        },
    )?;

    assert_eq!(memory.objective, "Recover failed task");
    assert!(
        memory
            .source_event_ids
            .contains(&"event-legacy-task".to_owned())
    );
    assert!(memory.unresolved_issues.iter().any(|issue| {
        issue.text == "missing check"
            && issue.source_event_id.as_deref() == Some("event-blocked-step")
    }));
    assert!(memory.failed_attempts.iter().any(|attempt| {
        attempt.attempt_id == "task-legacy" && attempt.summary.as_deref() == Some("planner failed")
    }));
    assert!(memory.failed_attempts.iter().any(|attempt| {
        attempt.attempt_id == "task-legacy:verify"
            && attempt.summary.as_deref() == Some("missing check")
    }));
    assert!(memory.failed_attempts.iter().any(|attempt| {
        attempt.attempt_id == "changeset-1"
            && attempt.summary.as_deref() == Some("changeset failed")
    }));
    assert!(memory.files_changed.iter().any(|file| {
        file.path == std::path::Path::new("src/lib.rs")
            && file.mutation_receipt_id.as_deref() == Some("changeset-1")
    }));
    Ok(())
}

#[test]
fn compaction_extraction_handles_directory_mutation_and_invalid_payload() -> Result<()> {
    let directory_mutation = MutationCommitted {
        operation_id: "op-dir".to_owned(),
        batch_id: None,
        workspace_id: Some("workspace-1".to_owned()),
        observed_after_hash: None,
        workspace_revision: 7,
        workspace_snapshot_id: "snapshot-dir".to_owned(),
        committed_subject: MutationSubject::Directory {
            path: "generated".into(),
        },
    };
    let memory = extract_task_memory_from_stream_records(
        &[stored_mutation_committed(
            1,
            "event-dir",
            directory_mutation,
        )?],
        TaskMemoryExtractionInput {
            memory_id: "memory-dir".to_owned(),
            valid_for_snapshot: "snapshot-dir".to_owned(),
            branch_id: None,
            supersedes: None,
            objective: Some("Track generated directory".to_owned()),
        },
    )?;
    assert!(memory.files_changed.iter().any(|file| {
        file.path == std::path::Path::new("generated")
            && file.mutation_receipt_id.as_deref() == Some("op-dir")
    }));

    let invalid = StoredEvent::new(
        DurableEventType::SessionEntryRecorded,
        EventClass::NonCritical,
        "event-invalid".to_owned(),
        "session-1".to_owned(),
        2,
        json!({ "session_log_entry": {"invalid": true} }),
    )?;
    let error = extract_task_memory_from_stream_records(
        &[SessionStreamRecord::Stored(invalid)],
        TaskMemoryExtractionInput {
            memory_id: "memory-invalid".to_owned(),
            valid_for_snapshot: "snapshot-invalid".to_owned(),
            branch_id: None,
            supersedes: None,
            objective: Some("Invalid".to_owned()),
        },
    )
    .expect_err("invalid stored session payload should fail");
    assert!(error.to_string().contains("unknown variant"));
    Ok(())
}

fn stored_session_entry(
    sequence: u64,
    event_id: &str,
    entry: SessionLogEntry,
) -> Result<SessionStreamRecord> {
    let event = StoredEvent::new(
        DurableEventType::SessionEntryRecorded,
        EventClass::NonCritical,
        event_id.to_owned(),
        "session-1".to_owned(),
        sequence,
        json!({ "session_log_entry": entry }),
    )?;
    Ok(SessionStreamRecord::Stored(event))
}

fn legacy_session_entry(
    sequence: u64,
    event_id: &str,
    entry: SessionLogEntry,
) -> SessionStreamRecord {
    SessionStreamRecord::Legacy {
        event: LegacyEvent {
            event_id: event_id.to_owned(),
            session_id: "session-1".to_owned(),
            stream_sequence: sequence,
            raw_line_hash: format!("sha256:{event_id}"),
            payload: json!({ "legacy": true }),
        },
        entry: Box::new(entry),
    }
}

fn stored_mutation_committed(
    sequence: u64,
    event_id: &str,
    mutation: MutationCommitted,
) -> Result<SessionStreamRecord> {
    let event = StoredEvent::new(
        DurableEventType::MutationCommitted,
        EventClass::Critical,
        event_id.to_owned(),
        "session-1".to_owned(),
        sequence,
        serde_json::to_value(mutation)?,
    )?;
    Ok(SessionStreamRecord::Stored(event))
}

fn verification_receipt(receipt_id: &str, source_event_id: &str) -> VerificationReceipt {
    VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id: receipt_id.to_owned(),
            source_session_id: "session-1".to_owned(),
            source_event_id: source_event_id.to_owned(),
            source_event_type: DurableEventType::VerificationRecorded.as_str().to_owned(),
            scope: EvidenceScope::Task("task-1".to_owned()),
            producer_tool_call: Some("call-bash".to_owned()),
            workspace_revision: Some(3),
            workspace_snapshot_id: Some("snapshot-2".to_owned()),
            policy_hash: None,
            changeset_id: None,
            status: ReceiptStatus::Succeeded,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence: 3,
        },
        binding: VerificationBinding {
            workspace_id: "workspace-1".to_owned(),
            workspace_snapshot_id: "snapshot-2".to_owned(),
            verification_scope_hash: "scope-hash".to_owned(),
            check_spec_hash: "check-hash".to_owned(),
            environment_fingerprint: "env".to_owned(),
            sandbox_profile_hash: "sandbox".to_owned(),
            execution_backend: None,
            execution_backend_capabilities: None,
            execution_network: Default::default(),
            workspace_trust_snapshot_id: "trust-snapshot".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        check_spec_id: "check".to_owned(),
        check_status: ReceiptStatus::Succeeded,
        failure_reason: None,
        mutates_verification_scope: false,
    }
}
