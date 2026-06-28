use anyhow::Result;
use serde_json::json;

use crate::{
    DurableEventType, EvalCase, EvalCaseRunner, EvalCaseRunnerOptions, EvalEvidenceKind,
    EvalEvidenceRef, EvalFailure, EvalFailureKind, EvalFakeToolAction, EvalFakeToolRegistry,
    EvalOutcomeKind, EvalProviderScript, EvalProviderStep, EvalRequiredActionKind, EvalResult,
    EvalRunMetadata, EvalToolCallStatus, EvalToolCallSummary, EvalWorkspaceFixture, EventClass,
    JsonlSessionStore, MutationEventRecorder, MutationObservedState, MutationReconciled,
    MutationResolution, PermissionConfig, PermissionPolicy, PermissionPreset, ProjectionCursor,
    RunStatus, SessionStreamRecord, StoredEvent, ToolAccess, ToolCategory, ToolPreviewCapability,
    ToolSpec, ToolSubject, VerificationVerdict, VisibleCompletionState, bytes_hash,
};

#[test]
fn eval_outcome_distinguishes_verified_and_unverified_completion() {
    let metadata = EvalRunMetadata::deterministic("case-read", "run-1", "fixture");
    let verified = EvalResult::from_completion(
        metadata.clone(),
        RunStatus::Completed,
        VerificationVerdict::Passed,
        Vec::new(),
    );

    assert_eq!(verified.outcome, EvalOutcomeKind::VerifiedSuccess);
    assert_eq!(verified.visible_state, VisibleCompletionState::Verified);
    assert_eq!(verified.run_status, RunStatus::Completed);
    assert_eq!(verified.verification_verdict, VerificationVerdict::Passed);

    let unverified = EvalResult::from_completion(
        metadata,
        RunStatus::Completed,
        VerificationVerdict::Missing,
        Vec::new(),
    );

    assert_eq!(unverified.outcome, EvalOutcomeKind::CompletedUnverified);
    assert_eq!(
        unverified.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    assert_eq!(unverified.run_status, RunStatus::Completed);
    assert_eq!(
        unverified.verification_verdict,
        VerificationVerdict::Missing
    );
}

#[test]
fn eval_outcome_preserves_permission_and_sandbox_denials() {
    let metadata = EvalRunMetadata::deterministic("case-deny", "run-1", "fixture");

    let permission = EvalResult::from_completion(
        metadata.clone(),
        RunStatus::Blocked,
        VerificationVerdict::Missing,
        vec![EvalFailure::new(
            EvalFailureKind::PermissionDenied,
            "read-only capability denied write",
        )],
    );

    assert_eq!(permission.outcome, EvalOutcomeKind::PermissionDenied);
    assert_eq!(
        permission.failures[0].kind,
        EvalFailureKind::PermissionDenied
    );

    let sandbox = EvalResult::from_completion(
        metadata,
        RunStatus::Failed,
        VerificationVerdict::Inconclusive,
        vec![EvalFailure::new(
            EvalFailureKind::SandboxDenied,
            "sandbox policy denied network",
        )],
    );

    assert_eq!(sandbox.outcome, EvalOutcomeKind::SandboxDenied);
    assert_eq!(sandbox.failures[0].kind, EvalFailureKind::SandboxDenied);
}

#[test]
fn eval_outcome_maps_remaining_terminal_states() {
    let metadata = EvalRunMetadata::deterministic("case-terminal", "run-1", "fixture");

    let failed_verification = EvalResult::from_completion(
        metadata.clone(),
        RunStatus::Failed,
        VerificationVerdict::Failed,
        Vec::new(),
    );
    assert_eq!(
        failed_verification.outcome,
        EvalOutcomeKind::FailedVerification
    );

    let failed = EvalResult::from_completion(
        metadata.clone(),
        RunStatus::Failed,
        VerificationVerdict::Inconclusive,
        Vec::new(),
    );
    assert_eq!(failed.outcome, EvalOutcomeKind::Failed);

    let cancelled = EvalResult::from_completion(
        metadata.clone(),
        RunStatus::Cancelled,
        VerificationVerdict::Missing,
        Vec::new(),
    );
    assert_eq!(cancelled.outcome, EvalOutcomeKind::Cancelled);

    let interrupted = EvalResult::from_completion(
        metadata.clone(),
        RunStatus::Interrupted,
        VerificationVerdict::Missing,
        Vec::new(),
    );
    assert_eq!(interrupted.outcome, EvalOutcomeKind::Interrupted);

    let running = EvalResult::from_completion(
        metadata.clone(),
        RunStatus::Paused,
        VerificationVerdict::Missing,
        Vec::new(),
    );
    assert_eq!(running.outcome, EvalOutcomeKind::Blocked);

    let running = EvalResult::from_completion(
        metadata,
        RunStatus::Running,
        VerificationVerdict::Pending,
        Vec::new(),
    );
    assert_eq!(running.outcome, EvalOutcomeKind::Blocked);
}

#[test]
fn eval_result_serializes_provider_neutral_metadata_and_evidence_refs() {
    let mut result = EvalResult::from_completion(
        EvalRunMetadata::deterministic("case-1", "run-1", "fixture-a"),
        RunStatus::Completed,
        VerificationVerdict::Missing,
        vec![EvalFailure::new(
            EvalFailureKind::VerificationMissing,
            "required check was not run",
        )],
    );
    result.changed_files.push("note.txt".into());
    result.approval_count = 1;
    result.session_log_path = Some("sessions/session.jsonl".into());
    result.durable_stream_cursor = Some(ProjectionCursor {
        session_id: "session-1".to_owned(),
        projection_schema_version: 1,
        last_applied_stream_sequence: 42,
        last_applied_event_id: "event-42".to_owned(),
        last_applied_record_checksum: "sha256:jcs-v1:abc".to_owned(),
    });
    result.tool_calls.push(EvalToolCallSummary {
        tool_call_id: "tool-1".to_owned(),
        tool_name: "write_file".to_owned(),
        status: EvalToolCallStatus::Succeeded,
    });
    result.evidence.push(EvalEvidenceRef::durable_event(
        "readiness-1",
        "event-readiness-1",
    ));

    let value = serde_json::to_value(&result).expect("eval result serializes");

    assert_eq!(value["metadata"]["provider"], "fake");
    assert_eq!(value["metadata"]["model"], "deterministic");
    assert_eq!(value["outcome"], "completed_unverified");
    assert_eq!(value["run_status"], "completed");
    assert_eq!(value["verification_verdict"], "missing");
    assert_eq!(value["visible_state"], "completed_unverified");
    assert_eq!(value["changed_files"][0], "note.txt");
    assert_eq!(value["tool_calls"][0]["tool_name"], "write_file");
    assert_eq!(value["tool_calls"][0]["status"], "succeeded");
    assert_eq!(value["approval_count"], 1);
    assert_eq!(value["evidence"][0]["kind"], "durable_event");
    assert_eq!(value["evidence"][0]["event_id"], "event-readiness-1");

    let restored: EvalResult = serde_json::from_value(value).expect("eval result deserializes");
    assert_eq!(restored, result);
}

#[test]
fn eval_evidence_ref_can_point_to_non_event_artifacts_without_body() {
    let evidence = EvalEvidenceRef {
        kind: EvalEvidenceKind::Artifact,
        id: "artifact-1".to_owned(),
        event_id: None,
        artifact_ref: Some("sha256:artifact".to_owned()),
    };

    let value = serde_json::to_value(&evidence).expect("evidence ref serializes");

    assert_eq!(value["kind"], "artifact");
    assert_eq!(value["id"], "artifact-1");
    assert_eq!(value["artifact_ref"], "sha256:artifact");
    assert!(value.get("event_id").is_none());
}

#[test]
fn eval_runner_reports_unregistered_fake_tool_as_tool_failure() {
    let case = EvalCase::deterministic(
        "case-unregistered-tool",
        "call a missing fake tool",
        EvalWorkspaceFixture::new("fixture-unregistered"),
        EvalProviderScript::new(vec![EvalProviderStep::ToolCall {
            tool_call_id: "call-missing-tool".to_owned(),
            tool_name: "missing_fake_tool".to_owned(),
            args_json: "{}".to_owned(),
        }]),
    );
    let runner = EvalCaseRunner::new(EvalFakeToolRegistry::new());

    let result = runner
        .run(case)
        .expect("unregistered fake tool is captured");

    assert_eq!(result.run_status, RunStatus::Failed);
    assert_eq!(result.outcome, EvalOutcomeKind::Failed);
    assert_eq!(result.failures[0].kind, EvalFailureKind::Tool);
    assert!(
        result.failures[0]
            .message
            .contains("fake tool missing_fake_tool is not registered")
    );
    assert_eq!(result.tool_calls[0].status, EvalToolCallStatus::Failed);
}

#[test]
fn eval_runner_records_tool_result_continuation_and_interrupts_unfinished_script() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let case = EvalCase::deterministic(
        "case-unfinished-script",
        "provider script stops before final answer",
        EvalWorkspaceFixture::new("fixture-unfinished"),
        EvalProviderScript::new(vec![
            EvalProviderStep::AssistantText {
                text: "starting".to_owned(),
            },
            EvalProviderStep::ToolResultContinuation {
                tool_call_id: "call-existing".to_owned(),
                text: "continued observation".to_owned(),
            },
        ]),
    );
    let runner = EvalCaseRunner::new(EvalFakeToolRegistry::new()).with_options(
        EvalCaseRunnerOptions::with_workspace_root(workspace_root.clone()),
    );

    let result = runner.run(case).expect("unfinished script is captured");

    assert_eq!(result.run_status, RunStatus::Interrupted);
    assert_eq!(result.outcome, EvalOutcomeKind::Interrupted);
    assert_eq!(result.failures[0].kind, EvalFailureKind::Harness);
    let session_log_path = result.session_log_path.as_ref().expect("session log path");
    let session_log = std::fs::read_to_string(session_log_path).expect("session log readable");
    assert!(session_log.contains("tool_result_continuation"));
    assert!(session_log.contains("call-existing"));
}

#[test]
fn eval_runner_replays_fake_provider_and_tools_deterministically() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let fixture = EvalWorkspaceFixture::new("fixture-basic").with_file("note.txt", "old");
    let script = EvalProviderScript::new(vec![
        EvalProviderStep::AssistantText {
            text: "I will edit the file".to_owned(),
        },
        EvalProviderStep::ToolCall {
            tool_call_id: "call-write".to_owned(),
            tool_name: "write_note".to_owned(),
            args_json: "{\"path\":\"note.txt\"}".to_owned(),
        },
        EvalProviderStep::ToolCall {
            tool_call_id: "call-check".to_owned(),
            tool_name: "check_note".to_owned(),
            args_json: "{}".to_owned(),
        },
        EvalProviderStep::FinalAnswer {
            text: "done".to_owned(),
        },
    ]);
    let case = EvalCase::deterministic("case-basic", "change old to new", fixture, script);
    let runner = EvalCaseRunner::new(
        EvalFakeToolRegistry::new()
            .with_tool(
                "write_note",
                EvalFakeToolAction::ControlledWriteSuccess {
                    path: "note.txt".into(),
                    content: "new".to_owned(),
                },
            )
            .with_tool(
                "check_note",
                EvalFakeToolAction::CheckSuccess {
                    check_id: "note-check".to_owned(),
                },
            ),
    )
    .with_options(EvalCaseRunnerOptions::with_workspace_root(
        workspace_root.clone(),
    ));

    let first = runner.run(case.clone()).expect("first eval run");
    let second = runner.run(case).expect("second eval run");

    assert_eq!(first, second);
    assert_eq!(first.outcome, EvalOutcomeKind::VerifiedSuccess);
    assert_eq!(first.run_status, RunStatus::Completed);
    assert_eq!(first.verification_verdict, VerificationVerdict::Passed);
    assert_eq!(
        first.changed_files,
        vec![std::path::PathBuf::from("note.txt")]
    );
    assert_eq!(first.tool_calls.len(), 2);
    assert_eq!(
        first.session_log_path.as_ref(),
        Some(&workspace_root.join("session.jsonl"))
    );
    assert!(
        first
            .session_log_path
            .as_ref()
            .is_some_and(|path| path.exists())
    );
    assert!(first.durable_stream_cursor.is_some());
    assert!(!first.evidence.is_empty());
}

#[test]
fn eval_runner_returns_structured_permission_denial_without_real_provider() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let case = EvalCase::deterministic(
        "case-denied",
        "try a denied write",
        EvalWorkspaceFixture::new("fixture-denied"),
        EvalProviderScript::new(vec![EvalProviderStep::ToolCall {
            tool_call_id: "call-denied".to_owned(),
            tool_name: "dangerous_write".to_owned(),
            args_json: "{}".to_owned(),
        }]),
    );
    let runner = EvalCaseRunner::new(EvalFakeToolRegistry::new().with_tool(
        "dangerous_write",
        EvalFakeToolAction::PermissionDenied {
            message: "write capability denied".to_owned(),
        },
    ))
    .with_options(EvalCaseRunnerOptions::with_workspace_root(
        workspace_root.clone(),
    ));

    let result = runner
        .run(case)
        .expect("permission denial is an eval result");

    assert_eq!(result.outcome, EvalOutcomeKind::PermissionDenied);
    assert_eq!(result.run_status, RunStatus::Blocked);
    assert_eq!(result.tool_calls[0].status, EvalToolCallStatus::Denied);
    assert_eq!(result.failures[0].kind, EvalFailureKind::PermissionDenied);
    assert!(
        result
            .session_log_path
            .as_ref()
            .is_some_and(|path| path.exists())
    );
}

#[test]
fn eval_runner_preserves_session_log_for_provider_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let case = EvalCase::deterministic(
        "case-provider-error",
        "trigger provider error",
        EvalWorkspaceFixture::new("fixture-error"),
        EvalProviderScript::new(vec![EvalProviderStep::ProviderError {
            message: "provider refused scripted request".to_owned(),
        }]),
    );
    let runner = EvalCaseRunner::new(EvalFakeToolRegistry::new()).with_options(
        EvalCaseRunnerOptions::with_workspace_root(workspace_root.clone()),
    );

    let result = runner.run(case).expect("provider error is captured");

    assert_eq!(result.outcome, EvalOutcomeKind::Failed);
    assert_eq!(result.run_status, RunStatus::Failed);
    assert_eq!(result.failures[0].kind, EvalFailureKind::Model);
    assert!(
        result
            .session_log_path
            .as_ref()
            .is_some_and(|path| path.exists())
    );
    assert!(result.durable_stream_cursor.is_some());
}

#[test]
fn eval_read_only_completion_is_not_forced_into_verification() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let case = EvalCase::deterministic(
        "case-read-only-with-configured-check",
        "inspect README positioning without modifying files",
        EvalWorkspaceFixture::new("fixture-read-only").with_file("README.md", "# Sigil\n"),
        EvalProviderScript::new(vec![
            EvalProviderStep::ToolCall {
                tool_call_id: "call-read".to_owned(),
                tool_name: "read_readme".to_owned(),
                args_json: "{\"path\":\"README.md\"}".to_owned(),
            },
            EvalProviderStep::FinalAnswer {
                text: "README positioning is clear".to_owned(),
            },
        ]),
    );
    let runner = EvalCaseRunner::new(
        EvalFakeToolRegistry::new()
            .with_tool(
                "read_readme",
                EvalFakeToolAction::ReadOnlySuccess {
                    output: "# Sigil".to_owned(),
                },
            )
            .with_tool(
                "configured_check_not_needed",
                EvalFakeToolAction::CheckSuccess {
                    check_id: "global-check".to_owned(),
                },
            ),
    )
    .with_options(EvalCaseRunnerOptions::with_workspace_root(
        workspace_root.clone(),
    ));

    let result = runner.run(case).expect("read-only eval run");

    assert_eq!(result.run_status, RunStatus::Completed);
    assert_eq!(
        result.verification_verdict,
        VerificationVerdict::NotApplicable
    );
    assert_eq!(result.visible_state, VisibleCompletionState::Completed);
    assert_eq!(result.outcome, EvalOutcomeKind::Completed);
    assert!(result.changed_files.is_empty());
    assert!(result.failures.is_empty());
    assert!(
        result
            .tool_calls
            .iter()
            .all(|call| call.tool_name != "configured_check_not_needed")
    );
    let session_log_path = result.session_log_path.as_ref().expect("session log path");
    let session_log = std::fs::read_to_string(session_log_path).expect("session log readable");
    assert!(!session_log.contains("mutation_prepared"));
    assert!(!session_log.contains("mutation_committed"));
    assert!(result.durable_stream_cursor.is_some());
}

#[test]
fn eval_write_missing_keeps_execution_and_verification_separate() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let case = EvalCase::deterministic(
        "case-write-missing",
        "change note.txt old to new",
        EvalWorkspaceFixture::new("fixture-write-missing").with_file("note.txt", "old"),
        EvalProviderScript::new(vec![
            EvalProviderStep::ToolCall {
                tool_call_id: "call-write".to_owned(),
                tool_name: "write_note".to_owned(),
                args_json: "{\"path\":\"note.txt\"}".to_owned(),
            },
            EvalProviderStep::FinalAnswer {
                text: "changed old to new".to_owned(),
            },
        ]),
    );
    let runner = EvalCaseRunner::new(EvalFakeToolRegistry::new().with_tool(
        "write_note",
        EvalFakeToolAction::ControlledWriteSuccess {
            path: "note.txt".into(),
            content: "new".to_owned(),
        },
    ))
    .with_options(EvalCaseRunnerOptions::with_workspace_root(
        workspace_root.clone(),
    ));

    let result = runner.run(case).expect("write-missing eval run");

    assert_eq!(result.run_status, RunStatus::Completed);
    assert_eq!(result.verification_verdict, VerificationVerdict::Missing);
    assert_eq!(
        result.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    assert_eq!(result.outcome, EvalOutcomeKind::CompletedUnverified);
    assert_eq!(
        result.changed_files,
        vec![std::path::PathBuf::from("note.txt")]
    );
    assert!(result.failures.is_empty());
    assert_eq!(result.required_actions.len(), 1);
    assert_eq!(
        result.required_actions[0].kind,
        EvalRequiredActionKind::RunCheck
    );
    assert!(
        result
            .evidence
            .iter()
            .any(|evidence| evidence.id == "controlled_write")
    );
    let session_log_path = result.session_log_path.as_ref().expect("session log path");
    let session_log = std::fs::read_to_string(session_log_path).expect("session log readable");
    assert!(session_log.contains("controlled_write"));
}

#[test]
fn eval_stale_after_later_write_points_to_invalidating_mutation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let case = EvalCase::deterministic(
        "case-stale-after-write",
        "write note, verify it, then change it again",
        EvalWorkspaceFixture::new("fixture-stale").with_file("note.txt", "old"),
        EvalProviderScript::new(vec![
            EvalProviderStep::ToolCall {
                tool_call_id: "call-write-a".to_owned(),
                tool_name: "write_a".to_owned(),
                args_json: "{\"path\":\"note.txt\"}".to_owned(),
            },
            EvalProviderStep::ToolCall {
                tool_call_id: "call-check".to_owned(),
                tool_name: "check_note".to_owned(),
                args_json: "{}".to_owned(),
            },
            EvalProviderStep::ToolCall {
                tool_call_id: "call-write-b".to_owned(),
                tool_name: "write_b".to_owned(),
                args_json: "{\"path\":\"note.txt\"}".to_owned(),
            },
            EvalProviderStep::FinalAnswer {
                text: "updated after verification".to_owned(),
            },
        ]),
    );
    let runner = EvalCaseRunner::new(
        EvalFakeToolRegistry::new()
            .with_tool(
                "write_a",
                EvalFakeToolAction::ControlledWriteSuccess {
                    path: "note.txt".into(),
                    content: "new".to_owned(),
                },
            )
            .with_tool(
                "check_note",
                EvalFakeToolAction::CheckSuccess {
                    check_id: "note-check".to_owned(),
                },
            )
            .with_tool(
                "write_b",
                EvalFakeToolAction::ControlledWriteSuccess {
                    path: "note.txt".into(),
                    content: "newer".to_owned(),
                },
            ),
    )
    .with_options(EvalCaseRunnerOptions::with_workspace_root(
        workspace_root.clone(),
    ));

    let result = runner.run(case).expect("stale eval run");

    assert_eq!(result.run_status, RunStatus::Completed);
    assert_eq!(result.verification_verdict, VerificationVerdict::Stale);
    assert_eq!(
        result.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    assert_eq!(result.outcome, EvalOutcomeKind::CompletedUnverified);
    assert_eq!(
        result.required_actions[0].kind,
        EvalRequiredActionKind::RunCheck
    );
    assert!(result.failures.iter().any(|failure| {
        failure.kind == EvalFailureKind::VerificationStale
            && failure
                .evidence
                .iter()
                .any(|evidence| evidence.id == "controlled_write")
    }));
    let session_log_path = result.session_log_path.as_ref().expect("session log path");
    let session_log = std::fs::read_to_string(session_log_path).expect("session log readable");
    assert!(session_log.contains("\"workspace_snapshot_id\":\"snapshot-1\""));
    assert_eq!(
        session_log
            .matches("\"label\":\"controlled_write\"")
            .count(),
        2
    );
}

#[test]
fn eval_integrity_reports_durable_stream_fail_closed_diagnostics() -> Result<()> {
    let temp = tempfile::tempdir()?;

    let checksum_path = temp.path().join("checksum.jsonl");
    let mut checksum_event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-checksum".to_owned(),
        "session-checksum".to_owned(),
        1,
        json!({ "call_id": "call-checksum" }),
    )?;
    checksum_event.record_checksum = "sha256:jcs-v1:wrong".to_owned();
    std::fs::write(
        &checksum_path,
        format!("{}\n", serde_json::to_string(&checksum_event)?),
    )?;
    let checksum_error = JsonlSessionStore::read_event_records(&checksum_path)
        .expect_err("checksum mismatch should fail closed");
    let checksum_result =
        integrity_failure_result("eval-integrity-checksum", &checksum_path, checksum_error);
    assert_eq!(checksum_result.outcome, EvalOutcomeKind::Failed);
    assert_eq!(checksum_result.failures[0].kind, EvalFailureKind::Integrity);
    assert!(checksum_result.failures[0].message.contains("checksum"));

    let gap_path = temp.path().join("gap.jsonl");
    let gap_event = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-gap".to_owned(),
        "session-gap".to_owned(),
        2,
        json!({ "call_id": "call-gap" }),
    )?;
    std::fs::write(&gap_path, gap_event.to_json_line()?)?;
    let gap_error =
        JsonlSessionStore::read_event_records(&gap_path).expect_err("sequence gap should fail");
    let gap_result = integrity_failure_result("eval-integrity-gap", &gap_path, gap_error);
    assert!(gap_result.failures[0].message.contains("sequence"));

    let middle_path = temp.path().join("middle-corruption.jsonl");
    let first = StoredEvent::new(
        DurableEventType::ToolExecutionStarted,
        EventClass::Critical,
        "event-first".to_owned(),
        "session-middle".to_owned(),
        1,
        json!({ "call_id": "call-first" }),
    )?;
    let second = StoredEvent::new(
        DurableEventType::ToolExecutionFinished,
        EventClass::Critical,
        "event-second".to_owned(),
        "session-middle".to_owned(),
        2,
        json!({ "call_id": "call-second" }),
    )?;
    std::fs::write(
        &middle_path,
        format!(
            "{}{{broken-json\n{}",
            first.to_json_line()?,
            second.to_json_line()?
        ),
    )?;
    let middle_error = JsonlSessionStore::read_event_records(&middle_path)
        .expect_err("middle corruption should fail closed");
    let middle_result = integrity_failure_result(
        "eval-integrity-middle-corruption",
        &middle_path,
        middle_error,
    );
    assert!(
        middle_result.failures[0].message.contains("parse")
            || middle_result.failures[0].message.contains("JSON")
            || middle_result.failures[0].message.contains("corruption")
    );

    let unknown_path = temp.path().join("unknown-critical.jsonl");
    let unknown = StoredEvent::new_raw(
        "future_critical_event",
        EventClass::Critical,
        "event-unknown".to_owned(),
        "session-unknown".to_owned(),
        1,
        json!({ "payload": true }),
    )?;
    std::fs::write(&unknown_path, unknown.to_json_line()?)?;
    let unknown_error = JsonlSessionStore::read_event_records(&unknown_path)
        .expect_err("unknown critical event should fail closed");
    let unknown_result = integrity_failure_result(
        "eval-integrity-unknown-critical",
        &unknown_path,
        unknown_error,
    );
    assert!(
        unknown_result.failures[0]
            .message
            .contains("unknown critical event")
    );

    Ok(())
}

#[test]
fn eval_integrity_reconciles_prepared_without_commit_and_partial_changeset() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());

    let not_applied_path = workspace.join("not-applied.txt");
    std::fs::write(&not_applied_path, "old")?;
    let not_applied = recorder.coordinator(&workspace, "call-not-applied", None)?;
    let not_applied_prepared = not_applied.prepare_file(
        "not-applied.txt",
        &not_applied_path,
        Some(bytes_hash(b"new")),
    )?;

    let missing_commit_path = workspace.join("missing-commit.txt");
    std::fs::write(&missing_commit_path, "old")?;
    let missing_commit = recorder.coordinator(&workspace, "call-missing-commit", None)?;
    let missing_commit_prepared = missing_commit.prepare_file(
        "missing-commit.txt",
        &missing_commit_path,
        Some(bytes_hash(b"new")),
    )?;
    std::fs::write(&missing_commit_path, "new")?;

    recorder.append_batch_started("batch-partial", "partial changeset", &[])?;
    let batch_applied_path = workspace.join("batch-applied.txt");
    std::fs::write(&batch_applied_path, "old")?;
    let batch_applied = recorder.coordinator(
        &workspace,
        "call-batch-applied",
        Some("batch-partial".to_owned()),
    )?;
    let batch_applied_prepared = batch_applied.prepare_file(
        "batch-applied.txt",
        &batch_applied_path,
        Some(bytes_hash(b"new")),
    )?;
    std::fs::write(&batch_applied_path, "new")?;

    let batch_missing_path = workspace.join("batch-missing.txt");
    std::fs::write(&batch_missing_path, "old")?;
    let batch_missing = recorder.coordinator(
        &workspace,
        "call-batch-missing",
        Some("batch-partial".to_owned()),
    )?;
    let batch_missing_prepared = batch_missing.prepare_file(
        "batch-missing.txt",
        &batch_missing_path,
        Some(bytes_hash(b"new")),
    )?;

    let reconciled = recorder.reconcile_prepared_mutations(&workspace)?;
    let payloads = reconciled
        .iter()
        .map(|event| serde_json::from_value::<MutationReconciled>(event.payload.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(payloads.len(), 4);
    assert_reconciled(
        &payloads,
        &not_applied_prepared.operation_id,
        MutationObservedState::NotApplied,
        MutationResolution::MarkNotApplied,
    );
    assert_reconciled(
        &payloads,
        &missing_commit_prepared.operation_id,
        MutationObservedState::AppliedAsIntended,
        MutationResolution::MarkCommitted,
    );
    assert_reconciled(
        &payloads,
        &batch_applied_prepared.operation_id,
        MutationObservedState::AppliedAsIntended,
        MutationResolution::MarkCommitted,
    );
    assert_reconciled(
        &payloads,
        &batch_missing_prepared.operation_id,
        MutationObservedState::NotApplied,
        MutationResolution::MarkNotApplied,
    );

    let mut result = EvalResult::from_completion(
        EvalRunMetadata::deterministic(
            "eval-integrity-reconcile",
            "run-integrity-reconcile",
            "fixture-integrity",
        ),
        RunStatus::Completed,
        VerificationVerdict::NotApplicable,
        Vec::new(),
    );
    result.session_log_path = Some(store.path().to_path_buf());
    result.evidence = reconciled
        .iter()
        .map(|event| EvalEvidenceRef::durable_event("mutation_reconciled", &event.event_id))
        .collect();
    result.durable_stream_cursor = reconciled.last().map(|event| ProjectionCursor {
        session_id: event.session_id.clone(),
        projection_schema_version: 1,
        last_applied_stream_sequence: event.stream_sequence,
        last_applied_event_id: event.event_id.clone(),
        last_applied_record_checksum: event.record_checksum.clone(),
    });

    assert_eq!(result.outcome, EvalOutcomeKind::Completed);
    assert_eq!(result.evidence.len(), 4);
    assert!(result.durable_stream_cursor.is_some());
    assert_eq!(
        JsonlSessionStore::read_event_records(store.path())?
            .into_iter()
            .filter(|record| matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type == DurableEventType::MutationReconciled.as_str()
            ))
            .count(),
        4
    );

    Ok(())
}

fn integrity_failure_result(
    case_id: &str,
    session_log_path: &std::path::Path,
    error: anyhow::Error,
) -> EvalResult {
    let mut result = EvalResult::from_completion(
        EvalRunMetadata::deterministic(case_id, "run-integrity", "fixture-integrity"),
        RunStatus::Failed,
        VerificationVerdict::Inconclusive,
        vec![EvalFailure {
            kind: EvalFailureKind::Integrity,
            message: format!("{error:#}"),
            evidence: vec![EvalEvidenceRef {
                kind: EvalEvidenceKind::SessionLog,
                id: "session-log".to_owned(),
                event_id: None,
                artifact_ref: Some(session_log_path.display().to_string()),
            }],
        }],
    );
    result.session_log_path = Some(session_log_path.to_path_buf());
    result
}

fn assert_reconciled(
    payloads: &[MutationReconciled],
    operation_id: &str,
    observed_state: MutationObservedState,
    resolution: MutationResolution,
) {
    assert!(payloads.iter().any(|payload| {
        payload.operation_id == operation_id
            && payload.observed_state == observed_state
            && payload.resolution == resolution
    }));
}

#[cfg(unix)]
#[test]
fn eval_security_path_blocks_symlink_and_parent_escape_without_external_write() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(&outside)?;
    let outside_file = outside.join("secret.txt");
    std::fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.join("link"))?;

    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "call-escape", None)?;
    let symlink_error = coordinator
        .prepare_file("link", workspace.join("link"), Some(bytes_hash(b"changed")))
        .expect_err("symlink escape should be denied before write");
    let parent_error = coordinator
        .prepare_file(
            "../outside/secret.txt",
            workspace.join("../outside/secret.txt"),
            Some(bytes_hash(b"changed")),
        )
        .expect_err("parent path escape should be denied before write");

    let result = EvalResult::from_completion(
        EvalRunMetadata::deterministic(
            "eval-security-path-escape",
            "run-security-path-escape",
            "fixture-security",
        ),
        RunStatus::Blocked,
        VerificationVerdict::Inconclusive,
        vec![
            EvalFailure::new(EvalFailureKind::PathEscapeDenied, symlink_error.to_string()),
            EvalFailure::new(EvalFailureKind::PathEscapeDenied, parent_error.to_string()),
        ],
    );

    assert_eq!(result.outcome, EvalOutcomeKind::Blocked);
    assert!(result.failures.iter().all(|failure| {
        failure.kind == EvalFailureKind::PathEscapeDenied
            && (failure.message.contains("does not match workspace subject")
                || failure.message.contains("must not escape"))
    }));
    assert_eq!(std::fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[test]
fn eval_security_path_distinguishes_read_only_and_approval_denials() {
    let shell_spec = ToolSpec {
        name: "bash".to_owned(),
        description: "shell".to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::Shell,
        access: ToolAccess::Execute,
        preview: ToolPreviewCapability::None,
    };
    let read_only_config = PermissionConfig {
        preset: PermissionPreset::ReadOnly,
        ..PermissionConfig::default()
    };
    let read_only_policy = PermissionPolicy::new(&read_only_config);
    let decision = read_only_policy
        .decide(
            &shell_spec,
            "bash",
            vec![ToolSubject::path(
                "note.txt".to_owned(),
                "note.txt".to_owned(),
            )],
        )
        .expect("permission decision");
    assert_eq!(decision.mode, crate::ApprovalMode::Deny);

    let read_only_result = EvalResult::from_completion(
        EvalRunMetadata::deterministic(
            "eval-security-read-only-shell",
            "run-security-read-only-shell",
            "fixture-security",
        ),
        RunStatus::Blocked,
        VerificationVerdict::Inconclusive,
        vec![EvalFailure::new(
            EvalFailureKind::PermissionDenied,
            "read-only capability denied shell write redirection",
        )],
    );
    assert_eq!(read_only_result.outcome, EvalOutcomeKind::PermissionDenied);

    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_root = temp.path().join("workspace");
    let case = EvalCase::deterministic(
        "eval-security-approval-denial",
        "attempt a write that user denies",
        EvalWorkspaceFixture::new("fixture-security").with_file("note.txt", "old"),
        EvalProviderScript::new(vec![EvalProviderStep::ToolCall {
            tool_call_id: "call-denied".to_owned(),
            tool_name: "write_note".to_owned(),
            args_json: "{\"path\":\"note.txt\"}".to_owned(),
        }]),
    );
    let runner = EvalCaseRunner::new(EvalFakeToolRegistry::new().with_tool(
        "write_note",
        EvalFakeToolAction::PermissionDenied {
            message: "approval denied by user".to_owned(),
        },
    ))
    .with_options(EvalCaseRunnerOptions::with_workspace_root(workspace_root));

    let approval_result = runner.run(case).expect("approval denial eval");
    assert_eq!(approval_result.outcome, EvalOutcomeKind::PermissionDenied);
    assert_eq!(approval_result.run_status, RunStatus::Blocked);
    assert!(approval_result.changed_files.is_empty());
    assert_eq!(
        approval_result.tool_calls[0].status,
        EvalToolCallStatus::Denied
    );
}
