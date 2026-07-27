use anyhow::{Context, Result};
use tempfile::tempdir;

use super::*;
use crate::*;

fn setup_chat_layer(
    root: &Path,
    relative_path: &str,
    before: &str,
    after: &str,
) -> Result<(
    JsonlSessionStore,
    Session,
    IntentVersionRef,
    IntentExecutionId,
)> {
    let workspace = root.join("workspace");
    fs::create_dir_all(
        workspace
            .join(relative_path)
            .parent()
            .context("test file has a parent")?,
    )?;
    fs::write(workspace.join(relative_path), before)?;
    let store = JsonlSessionStore::new(root.join("session.jsonl"))?;
    let mut session = Session::load_from_store("test-provider", "test-model", store.clone())?;
    let workspace_id = stable_workspace_id(&workspace)?;
    let admission = admit_user_declared_root(
        &IntentAdmissionContextV1::initial(
            IntentStackId::new("stack-layer-chat")?,
            workspace_id.clone(),
            session.session_scope_id(),
        )?,
        UserDeclaredIntentV1 {
            title: "Update retry".to_owned(),
            statement: "Update retry behavior.".to_owned(),
            acceptance_criteria: vec![IntentProposalCriterionV1 {
                criterion_alias: "retry-check".to_owned(),
                statement: "Retry behavior is updated.".to_owned(),
                required: true,
            }],
        },
        &IntentAcceptanceAuthorityV1::user_declared_root("turn-layer", "event-turn-layer")?,
    )?;
    append_chat_root_intent_admission(&session, &admission)?;
    let intent_ref = admission.plan().intents[0].intent_ref.clone();
    session.append_control(ControlEntry::AgentRunAttemptStarted(
        AgentRunAttemptStartedEntry {
            thread_id: AgentThreadId::new("root-layer-run")?,
            attempt_id: AgentRunAttemptId::new("root-layer-attempt")?,
            provider: "test-provider".to_owned(),
            model: "test-model".to_owned(),
            background: false,
            provider_background_handle_ref: None,
        },
    ))?;
    let execution_id = append_chat_intent_execution_binding(
        &session,
        intent_ref.clone(),
        "root-layer-run",
        "turn-layer",
        "root-layer-attempt",
    )?
    .execution_id
    .context("execution id is present")?;

    let recorder = MutationEventRecorder::new(store.clone());
    let operation_id = "operation-layer-chat";
    let before_bytes = before.as_bytes();
    let after_bytes = after.as_bytes();
    let before_hash = IntentContentDigest::from_bytes(before_bytes);
    let after_hash = IntentContentDigest::from_bytes(after_bytes);
    let before_artifact = recorder.capture_immutable_content_artifact(
        &workspace_id,
        operation_id,
        Path::new(relative_path),
        before_bytes,
    )?;
    let prepared = recorder.append_prepared(&MutationPrepared {
        operation_id: operation_id.to_owned(),
        batch_id: None,
        tool_call_id: Some("tool-layer-chat".to_owned()),
        causation_event_id: "event-layer-tool".to_owned(),
        subject: MutationSubject::File {
            path: relative_path.into(),
            file_type: FileType::File,
        },
        before_hash: Some(before_hash.as_str().to_owned()),
        intended_after_hash: Some(after_hash.as_str().to_owned()),
        snapshot_coverage: SnapshotCoverage::Captured(before_artifact),
        workspace_id: workspace_id.clone(),
        base_workspace_revision: 0,
        sync_class: MutationSyncClass::RecoveryCritical,
    })?;
    fs::write(workspace.join(relative_path), after)?;
    let committed = recorder.append_committed(&MutationCommitted {
        operation_id: operation_id.to_owned(),
        batch_id: None,
        workspace_id: Some(workspace_id),
        observed_after_hash: Some(after_hash.as_str().to_owned()),
        workspace_revision: 1,
        workspace_snapshot_id: "snapshot-layer-after".to_owned(),
        committed_subject: MutationSubject::File {
            path: relative_path.into(),
            file_type: FileType::File,
        },
    })?;
    append_chat_direct_mutation_changeset_binding(
        &mut session,
        &execution_id,
        &prepared.event_id,
        &committed.event_id,
    )?;
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&ConversationRunStartedEntryV1::new("root-layer-run", 1)?)?;
    lifecycle.append_finalized(&ConversationRunFinalizedEntryV1::new(
        "root-layer-run",
        ConversationRunTerminalStatusV1::Succeeded,
        Some("assistant-layer-final".to_owned()),
        Some("completed"),
        2,
        &SecretRedactor::empty(),
    )?)?;
    Ok((store, session, intent_ref, execution_id))
}

fn setup_task_layer(
    root: &Path,
) -> Result<(
    JsonlSessionStore,
    Session,
    IntentVersionRef,
    IntentExecutionId,
)> {
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join("src"))?;
    let relative_path = "src/retry.rs";
    let before = b"fn retry() { 1 }\n";
    let after = b"fn retry() { 2 }\n";
    fs::write(workspace.join(relative_path), before)?;
    let store = JsonlSessionStore::new(root.join("session.jsonl"))?;
    let mut session = Session::load_from_store("test-provider", "test-model", store.clone())?;
    let workspace_id = stable_workspace_id(&workspace)?;
    let admission = admit_user_declared_root(
        &IntentAdmissionContextV1::initial(
            IntentStackId::new("stack-layer-task")?,
            workspace_id.clone(),
            session.session_scope_id(),
        )?,
        UserDeclaredIntentV1 {
            title: "Update retry".to_owned(),
            statement: "Update retry behavior.".to_owned(),
            acceptance_criteria: vec![IntentProposalCriterionV1 {
                criterion_alias: "retry-check".to_owned(),
                statement: "Retry behavior is updated.".to_owned(),
                required: true,
            }],
        },
        &IntentAcceptanceAuthorityV1::user_declared_root(
            "turn-layer-task",
            "event-turn-layer-task",
        )?,
    )?;
    let intent_ref = admission.plan().intents[0].intent_ref.clone();
    let task_id = TaskId::new("task-layer")?;
    let step_id = TaskStepId::new("implement-layer")?;
    let task_plan = bind_task_plan_intents(
        &admission,
        TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Implement retry".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            }],
            reason: None,
        },
        &[TaskStepIntentAliasBindingV1 {
            step_id: step_id.clone(),
            intent_aliases: vec![USER_DECLARED_ROOT_INTENT_ALIAS.to_owned()],
        }],
    )?;
    append_task_intent_plan_admission(&mut session, &admission, task_plan)?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    let attempt = TaskParticipantAttemptEntry {
        attempt_id: attempt_id.clone(),
        task_id: task_id.clone(),
        purpose: TaskParticipantPurpose::Step,
        ordinal: 1,
        plan_version: Some(1),
        step_id: Some(step_id.clone()),
        role: AgentRole::Executor,
        child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
        status: TaskParticipantAttemptStatus::Started,
        reason: None,
    };
    session.append_control(ControlEntry::TaskParticipantAttempt(attempt.clone()))?;
    let execution_id = append_task_intent_execution_binding(
        &session,
        intent_ref.clone(),
        &task_id,
        1,
        &step_id,
        &attempt_id,
    )?
    .execution_id
    .context("Task execution id is present")?;

    let before_hash = IntentContentDigest::from_bytes(before);
    let after_hash = IntentContentDigest::from_bytes(after);
    let change_set = ChangeSet {
        id: ChangeSetId::new("changeset-layer-task")?,
        title: "Retry implementation".to_owned(),
        summary: "Update retry behavior.".to_owned(),
        risk: ChangeSetRisk::Low,
        files: vec![ChangeSetFile {
            path: relative_path.to_owned(),
            previous_path: None,
            action: ChangeSetFileAction::Update,
            risk: ChangeSetRisk::Low,
            before_hash: Some(before_hash.as_str().to_owned()),
            after_hash: Some(after_hash.as_str().to_owned()),
            diff_hash: Some(format!("sha256:{}", "c".repeat(64))),
            additions: 1,
            deletions: 1,
            validations: Vec::new(),
        }],
        validations: Vec::new(),
    };
    session.append_control(ControlEntry::ChangeSetProposed(change_set.clone()))?;
    append_intent_changeset_binding(&session, &execution_id, vec![change_set.id.clone()])?;

    let proposal_facts = IntegrationProposalFacts::from_changeset(
        &change_set,
        IntegrationBaseRepresentation::CleanCommit {
            base_commit: "a".repeat(40),
        },
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        "artifact:changeset-layer-task",
        Vec::new(),
    )?;
    let proposal = IntegrationProposalSpec::from_changeset(
        &change_set,
        step_id,
        "snapshot-task-base".to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        "scope-task-layer",
        proposal_facts,
    )?;
    let integration_plan = build_integration_plan(
        IntegrationPlanId::new("integration-layer-task")?,
        task_id,
        1,
        vec![proposal],
    )?;
    session.append_control(ControlEntry::IntegrationPlanRecorded(
        IntegrationPlanRecorded {
            plan: integration_plan.clone(),
        },
    ))?;

    let recorder = MutationEventRecorder::new(store.clone());
    let operation_id = "operation-layer-task".to_owned();
    let batch_id = "batch-layer-task".to_owned();
    let subject = MutationSubject::File {
        path: relative_path.into(),
        file_type: FileType::File,
    };
    let before_artifact = recorder.capture_immutable_content_artifact(
        &workspace_id,
        &operation_id,
        Path::new(relative_path),
        before,
    )?;
    recorder.append_batch_started(
        &batch_id,
        "operation-layer-task-batch",
        std::slice::from_ref(&subject),
    )?;
    recorder.append_prepared(&MutationPrepared {
        operation_id: operation_id.clone(),
        batch_id: Some(batch_id.clone()),
        tool_call_id: Some("tool-layer-task".to_owned()),
        causation_event_id: "event-layer-task".to_owned(),
        subject: subject.clone(),
        before_hash: Some(before_hash.as_str().to_owned()),
        intended_after_hash: Some(after_hash.as_str().to_owned()),
        snapshot_coverage: SnapshotCoverage::Captured(before_artifact),
        workspace_id: workspace_id.clone(),
        base_workspace_revision: 1,
        sync_class: MutationSyncClass::RecoveryCritical,
    })?;
    fs::write(workspace.join(relative_path), after)?;
    recorder.append_committed(&MutationCommitted {
        operation_id: operation_id.clone(),
        batch_id: Some(batch_id.clone()),
        workspace_id: Some(workspace_id),
        observed_after_hash: Some(after_hash.as_str().to_owned()),
        workspace_revision: 2,
        workspace_snapshot_id: "snapshot-task-file".to_owned(),
        committed_subject: subject,
    })?;
    recorder.append_batch_finished(
        &batch_id,
        MutationBatchStatus::Applied,
        std::slice::from_ref(&operation_id),
        &[],
    )?;
    session.append_control(ControlEntry::ChangeSetApplied(ChangeSetResult {
        id: change_set.id,
        status: ChangeSetResultStatus::Applied,
        file_results: vec![ChangeSetFileResult {
            path: relative_path.to_owned(),
            action: ChangeSetFileAction::Update,
            status: ChangeSetFileResultStatus::Applied,
            message: None,
            validations: Vec::new(),
        }],
        message: None,
    }))?;
    session.append_control(ControlEntry::IntegrationPromotionRecorded(
        IntegrationPromotionRecorded {
            plan_id: integration_plan.plan_id,
            attempt_id: None,
            status: IntegrationPromotionStatus::Promoted,
            preview_digest: format!("sha256:{}", "d".repeat(64)),
            target: IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id: "snapshot-task-base".to_owned(),
                expected_revision: 1,
            },
            authority_nonce: None,
            effect: Some(IntegrationPromotionEffect::WorkspaceApplied {
                promoted_snapshot_id: "snapshot-task-promoted".to_owned(),
                promoted_revision: 2,
            }),
            recovery_binding: None,
            reason: None,
            recorded_at_unix_ms: 1,
        },
    ))?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            status: TaskParticipantAttemptStatus::Completed,
            ..attempt
        },
    ))?;
    Ok((store, session, intent_ref, execution_id))
}

#[test]
fn chat_layer_materializes_content_addressed_artifacts_and_replays() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, intent_ref, execution_id) = setup_chat_layer(
        temp.path(),
        "src/retry.rs",
        "fn retry() { 1 }\n",
        "fn retry() { 2 }\n",
    )?;
    let outcome = materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    assert!(outcome.appended);
    assert!(outcome.manifest_digest.is_some());

    let replay = session.intent_layer_projection()?;
    let layer = replay.layer(&execution_id).context("layer replays")?;
    assert!(
        layer
            .layer_manifest
            .core
            .forward_patch_artifact_id
            .starts_with("mutation-artifact:sha256:")
    );
    assert_eq!(layer.artifacts.len(), 1);
    assert_eq!(
        layer.artifacts[0].ownership,
        IntentArtifactOwnership::Exclusive
    );
    assert_eq!(
        replay.summary_for(&intent_ref).application_state,
        Some(IntentApplicationState::Applied)
    );

    let second = materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    assert!(!second.appended);
    assert_eq!(second.manifest_digest, outcome.manifest_digest);
    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { stack, .. }
            if stack.intents[0].application_state == IntentApplicationState::Applied
                && stack.intents[0].exclusive_artifact_count == 1
                && stack.intents[0].artifacts[0].normalized_relative_path.as_deref()
                    == Some("src/retry.rs")
    ));
    Ok(())
}

#[test]
fn task_workspace_apply_materializes_exact_parent_layer() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, intent_ref, execution_id) = setup_task_layer(temp.path())?;
    let outcome = materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    assert!(outcome.appended);
    let projection = session.intent_layer_projection()?;
    let layer = projection
        .layer(&execution_id)
        .context("Task layer exists")?;
    assert_eq!(
        layer.layer_manifest.core.base_snapshot_id,
        "snapshot-task-base"
    );
    assert_eq!(
        layer.layer_manifest.core.result_snapshot_id,
        "snapshot-task-promoted"
    );
    assert!(matches!(
        layer.layer_manifest.core.execution_origin,
        IntentExecutionOriginV1::Task { .. }
    ));
    assert_eq!(
        projection.summary_for(&intent_ref).application_state,
        Some(IntentApplicationState::Applied)
    );
    Ok(())
}

#[test]
fn canonical_patch_distinguishes_absent_file_from_empty_file() -> Result<()> {
    fn materialized(before: Option<Vec<u8>>) -> MaterializedIntentFile {
        let after = b"x\n".to_vec();
        MaterializedIntentFile {
            path: "src/new.rs".to_owned(),
            before_digest: IntentContentDigest::from_bytes(before.as_deref().unwrap_or_default()),
            after_digest: IntentContentDigest::from_bytes(&after),
            old_range: IntentByteRangeV1 { start: 0, end: 0 },
            new_range: IntentByteRangeV1 { start: 0, end: 2 },
            old_content_digest: IntentContentDigest::from_bytes([]),
            new_content_digest: IntentContentDigest::from_bytes(&after),
            old_changed_bytes: Vec::new(),
            new_changed_bytes: after.clone(),
            before,
            after: Some(after),
            source_event_id: "event-create".to_owned(),
            mutation_identity: "operation-create".to_owned(),
            changeset_id: "changeset-create".to_owned(),
        }
    }

    let absent = encode_patch(&[materialized(None)], PatchDirection::Forward)?;
    let empty = encode_patch(&[materialized(Some(Vec::new()))], PatchDirection::Forward)?;
    assert_ne!(absent, empty, "CAS must bind expected file presence");
    Ok(())
}

#[test]
fn later_exact_file_mutation_marks_materialized_layer_drifted() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref, execution_id) = setup_chat_layer(
        temp.path(),
        "src/retry.rs",
        "fn retry() { 1 }\n",
        "fn retry() { 2 }\n",
    )?;
    materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    MutationEventRecorder::new(store).append_committed(&MutationCommitted {
        operation_id: "formatter-after-layer".to_owned(),
        batch_id: None,
        workspace_id: Some(stable_workspace_id(temp.path().join("workspace"))?),
        observed_after_hash: Some(
            IntentContentDigest::from_bytes(b"formatted\n")
                .as_str()
                .to_owned(),
        ),
        workspace_revision: 2,
        workspace_snapshot_id: "snapshot-formatter".to_owned(),
        committed_subject: MutationSubject::File {
            path: "src/retry.rs".into(),
            file_type: FileType::File,
        },
    })?;
    let summary = session.intent_layer_projection()?.summary_for(&intent_ref);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::ReadOnly)
    );
    assert_eq!(summary.drifted_artifact_count, 1);
    assert_eq!(
        summary.read_only_reason,
        Some(IntentLayerReadOnlyReasonV1::DriftedArtifact)
    );
    Ok(())
}

#[test]
fn unknown_codegen_workspace_mutation_marks_materialized_layer_drifted() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref, execution_id) = setup_chat_layer(
        temp.path(),
        "src/generated.rs",
        "pub const VALUE: u8 = 1;\n",
        "pub const VALUE: u8 = 2;\n",
    )?;
    materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    MutationEventRecorder::new(store).append_workspace_mutation_detected(
        &WorkspaceMutationDetected {
            operation_id: "codegen-after-layer".to_owned(),
            tool_call_id: Some("tool-codegen".to_owned()),
            tool_name: "codegen".to_owned(),
            tool_effect: ToolEffect::Unknown,
            workspace_id: stable_workspace_id(temp.path().join("workspace"))?,
            scope_hash: "scope-codegen".to_owned(),
            from_workspace_snapshot_id: Some("snapshot-layer-after".to_owned()),
            to_workspace_snapshot_id: None,
            base_workspace_revision: 1,
            workspace_revision: 2,
            reason: WorkspaceMutationDetectionReason::ScanUnavailable,
            unknown_dirty: true,
            metadata: Default::default(),
        },
    )?;
    let summary = session.intent_layer_projection()?.summary_for(&intent_ref);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::ReadOnly)
    );
    assert_eq!(summary.drifted_artifact_count, 1);
    assert_eq!(
        summary.read_only_reason,
        Some(IntentLayerReadOnlyReasonV1::DriftedArtifact)
    );
    Ok(())
}

#[test]
fn secret_like_content_never_creates_executable_layer_artifacts() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, _intent_ref, execution_id) = setup_chat_layer(
        temp.path(),
        "src/config.rs",
        "let enabled = false;\n",
        "let api_key = \"super-secret-value\";\n",
    )?;
    let outcome = materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    assert_eq!(
        outcome.read_only_reason,
        Some(IntentLayerReadOnlyReasonV1::SensitiveContent)
    );
    assert!(
        session
            .intent_layer_projection()?
            .layer(&execution_id)
            .is_none()
    );
    Ok(())
}

#[test]
fn same_file_owned_by_two_active_intents_is_always_shared() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, first_intent_ref, first_execution_id) = setup_chat_layer(
        temp.path(),
        "src/retry.rs",
        "fn retry() { 1 }\n",
        "fn retry() { 2 }\n",
    )?;
    materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &first_execution_id,
        &SecretRedactor::empty(),
    )?;
    let admission = session.intent_stack_projection()?;
    let mut accepted = admission
        .latest_accepted_plan()
        .context("accepted plan exists")?
        .clone();
    let second_intent_ref = IntentVersionRef::new(IntentId::new("second-intent")?, 1)?;
    let mut second_intent = accepted.plan.intents[0].clone();
    second_intent.intent_ref = second_intent_ref.clone();
    second_intent.depends_on.clear();
    accepted.plan.intents.push(second_intent);

    let mut projection = session.intent_layer_projection()?;
    let second_execution_id = IntentExecutionId::new("execution-second-intent")?;
    let mut second_layer = projection
        .layer(&first_execution_id)
        .context("first layer exists")?
        .clone();
    second_layer.artifact_manifest.intent_ref = second_intent_ref.clone();
    second_layer.artifact_manifest.execution_id = second_execution_id.clone();
    second_layer.layer_manifest.core.intent_ref = second_intent_ref.clone();
    second_layer.layer_manifest.core.execution_id = second_execution_id.clone();
    for artifact in &mut second_layer.artifacts {
        artifact.binding.provenance.execution_id = second_execution_id.clone();
    }
    projection.layer_order.push(second_execution_id.clone());
    projection
        .layers
        .insert(second_execution_id.clone(), second_layer);
    projection.apply_shared_file_ownership(Some(&accepted));

    assert_eq!(
        projection
            .summary_for(&first_intent_ref)
            .shared_artifact_count,
        1
    );
    assert_eq!(
        projection
            .summary_for(&second_intent_ref)
            .shared_artifact_count,
        1
    );
    assert_eq!(
        projection.summary_for(&second_intent_ref).read_only_reason,
        Some(IntentLayerReadOnlyReasonV1::SharedOwnership)
    );
    Ok(())
}

#[test]
fn unowned_artifact_is_never_projected_as_executable() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, intent_ref, execution_id) = setup_chat_layer(
        temp.path(),
        "src/retry.rs",
        "fn retry() { 1 }\n",
        "fn retry() { 2 }\n",
    )?;
    materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    let mut projection = session.intent_layer_projection()?;
    projection
        .layers
        .get_mut(&execution_id)
        .context("layer exists")?
        .artifacts[0]
        .ownership = IntentArtifactOwnership::Unowned;
    let summary = projection.summary_for(&intent_ref);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::ReadOnly)
    );
    assert_eq!(summary.unowned_artifact_count, 1);
    assert_eq!(
        summary.read_only_reason,
        Some(IntentLayerReadOnlyReasonV1::UnownedArtifact)
    );
    Ok(())
}

#[test]
fn crash_prefix_without_final_layer_manifest_fails_closed() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, _intent_ref, execution_id) = setup_chat_layer(
        temp.path(),
        "src/retry.rs",
        "fn retry() { 1 }\n",
        "fn retry() { 2 }\n",
    )?;
    materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    let mut records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(
        records
            .pop()
            .context("final layer event exists")?
            .stored_event()
            .event_kind(),
        Some(DurableEventType::IntentLayerManifestRecorded)
    );
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let lineage = IntentLineageProjectionV1::from_records(&records, &admission)?;
    assert!(
        IntentLayerProjectionV1::from_records(&records, &admission, &lineage)
            .expect_err("orphan artifact manifest must fail closed")
            .to_string()
            .contains("missing its adjacent final layer")
    );
    Ok(())
}

#[test]
fn retention_protects_active_layer_but_explicit_delete_is_permanently_visible() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, intent_ref, execution_id) = setup_chat_layer(
        temp.path(),
        "src/retry.rs",
        "fn retry() { 1 }\n",
        "fn retry() { 2 }\n",
    )?;
    materialize_intent_layer(
        &session,
        temp.path().join("workspace"),
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    let recorder = session
        .mutation_event_recorder()
        .context("recorder is available")?;
    let protected = recorder.enforce_artifact_retention_at(
        &MutationArtifactRetentionPolicy {
            max_artifacts: Some(0),
            max_bytes: Some(0),
            expire_older_than_ms: Some(u64::MAX),
        },
        u64::MAX,
    )?;
    assert_eq!(protected.deleted_artifacts + protected.expired_artifacts, 1);
    let layer = session
        .intent_layer_projection()?
        .layer(&execution_id)
        .context("layer remains")?
        .clone();
    assert_eq!(
        session
            .intent_layer_projection()?
            .summary_for(&intent_ref)
            .application_state,
        Some(IntentApplicationState::Applied)
    );

    recorder.delete_mutation_artifact(
        layer.layer_manifest.core.reverse_patch_artifact_id.clone(),
        "user removed the executable reverse patch",
    )?;
    let reopened = Session::load_from_store(
        "test-provider",
        "test-model",
        JsonlSessionStore::new(temp.path().join("session.jsonl"))?,
    )?;
    let summary = reopened.intent_layer_projection()?.summary_for(&intent_ref);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::ReadOnly)
    );
    assert_eq!(summary.unavailable_artifact_count, 1);
    assert_eq!(
        summary.read_only_reason,
        Some(IntentLayerReadOnlyReasonV1::ArtifactUnavailable)
    );
    Ok(())
}
