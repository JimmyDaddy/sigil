use anyhow::{Context, Result};
use tempfile::tempdir;

use super::*;
use crate::*;

fn setup_chat_drop(
    root: &Path,
) -> Result<(
    JsonlSessionStore,
    Session,
    IntentVersionRef,
    IntentExecutionId,
)> {
    setup_chat_drop_contents(
        root,
        Some(b"fn retry() { 1 }\n"),
        Some(b"fn retry() { 2 }\n"),
    )
}

fn setup_chat_drop_contents(
    root: &Path,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
) -> Result<(
    JsonlSessionStore,
    Session,
    IntentVersionRef,
    IntentExecutionId,
)> {
    let workspace = root.join("workspace");
    let relative_path = "src/retry.rs";
    fs::create_dir_all(workspace.join("src"))?;
    if let Some(before) = before {
        fs::write(workspace.join(relative_path), before)?;
    }
    let store = JsonlSessionStore::new(root.join("session.jsonl"))?;
    let mut session = Session::load_from_store("test-provider", "test-model", store.clone())?;
    let workspace_id = stable_workspace_id(&workspace)?;
    let admission = admit_user_declared_root(
        &IntentAdmissionContextV1::initial(
            IntentStackId::new("stack-operation-chat")?,
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
        &IntentAcceptanceAuthorityV1::user_declared_root("turn-operation", "event-turn-operation")?,
    )?;
    append_chat_root_intent_admission(&session, &admission)?;
    let intent_ref = admission.plan().intents[0].intent_ref.clone();
    session.append_control(ControlEntry::AgentRunAttemptStarted(
        AgentRunAttemptStartedEntry {
            thread_id: AgentThreadId::new("root-operation-run")?,
            attempt_id: AgentRunAttemptId::new("root-operation-attempt")?,
            provider: "test-provider".to_owned(),
            model: "test-model".to_owned(),
            background: false,
            provider_background_handle_ref: None,
        },
    ))?;
    let execution_id = append_chat_intent_execution_binding(
        &session,
        intent_ref.clone(),
        "root-operation-run",
        "turn-operation",
        "root-operation-attempt",
    )?
    .execution_id
    .context("execution id is present")?;
    session.append_user_message(ModelMessage::user("update retry"))?;

    let recorder = MutationEventRecorder::new(store.clone());
    let operation_id = "operation-intent-source";
    let before_hash = before.map(IntentContentDigest::from_bytes);
    let after_hash = after.map(IntentContentDigest::from_bytes);
    let snapshot_coverage = match before {
        Some(before) => SnapshotCoverage::Captured(recorder.capture_immutable_content_artifact(
            &workspace_id,
            operation_id,
            Path::new(relative_path),
            before,
        )?),
        None => SnapshotCoverage::NoPriorContent,
    };
    let prepared = recorder.append_prepared(&MutationPrepared {
        operation_id: operation_id.to_owned(),
        batch_id: None,
        tool_call_id: Some("tool-operation-source".to_owned()),
        causation_event_id: "event-operation-source".to_owned(),
        subject: MutationSubject::File {
            path: relative_path.into(),
            file_type: FileType::File,
        },
        before_hash: before_hash
            .as_ref()
            .map(|digest| digest.as_str().to_owned()),
        intended_after_hash: after_hash.as_ref().map(|digest| digest.as_str().to_owned()),
        snapshot_coverage,
        workspace_id: workspace_id.clone(),
        base_workspace_revision: 0,
        sync_class: MutationSyncClass::RecoveryCritical,
    })?;
    match after {
        Some(after) => fs::write(workspace.join(relative_path), after)?,
        None => fs::remove_file(workspace.join(relative_path))?,
    }
    let committed = recorder.append_committed(&MutationCommitted {
        operation_id: operation_id.to_owned(),
        batch_id: None,
        workspace_id: Some(workspace_id),
        observed_after_hash: after_hash.as_ref().map(|digest| digest.as_str().to_owned()),
        workspace_revision: 1,
        workspace_snapshot_id: "snapshot-operation-after".to_owned(),
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
    lifecycle.append_started(&ConversationRunStartedEntryV1::new(
        "root-operation-run",
        1,
    )?)?;
    lifecycle.append_finalized(&ConversationRunFinalizedEntryV1::new(
        "root-operation-run",
        ConversationRunTerminalStatusV1::Succeeded,
        Some("assistant-operation-final".to_owned()),
        Some("completed"),
        2,
        &SecretRedactor::empty(),
    )?)?;
    materialize_intent_layer(
        &session,
        &workspace,
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    Ok((store, session, intent_ref, execution_id))
}

fn setup_task_multifile_drop(
    root: &Path,
) -> Result<(JsonlSessionStore, Session, IntentVersionRef)> {
    let workspace = root.join("workspace");
    fs::create_dir_all(workspace.join("src"))?;
    let files = [
        (
            "src/retry.rs",
            b"fn retry() { 1 }\n".as_slice(),
            b"fn retry() { 2 }\n".as_slice(),
        ),
        (
            "src/telemetry.rs",
            b"fn telemetry() { 1 }\n".as_slice(),
            b"fn telemetry() { 2 }\n".as_slice(),
        ),
    ];
    for (path, before, _) in files {
        fs::write(workspace.join(path), before)?;
    }
    let store = JsonlSessionStore::new(root.join("session.jsonl"))?;
    let mut session = Session::load_from_store("test-provider", "test-model", store.clone())?;
    let workspace_id = stable_workspace_id(&workspace)?;
    let admission = admit_user_declared_root(
        &IntentAdmissionContextV1::initial(
            IntentStackId::new("stack-operation-task")?,
            workspace_id.clone(),
            session.session_scope_id(),
        )?,
        UserDeclaredIntentV1 {
            title: "Update retry and telemetry".to_owned(),
            statement: "Update two implementation files.".to_owned(),
            acceptance_criteria: vec![IntentProposalCriterionV1 {
                criterion_alias: "multifile-check".to_owned(),
                statement: "Both files are updated.".to_owned(),
                required: true,
            }],
        },
        &IntentAcceptanceAuthorityV1::user_declared_root(
            "turn-operation-task",
            "event-turn-operation-task",
        )?,
    )?;
    let intent_ref = admission.plan().intents[0].intent_ref.clone();
    let task_id = TaskId::new("task-operation")?;
    let step_id = TaskStepId::new("implement-operation")?;
    let task_plan = bind_task_plan_intents(
        &admission,
        TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Implement both files".to_owned(),
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
    .context("Task execution id")?;
    let change_set = ChangeSet {
        id: ChangeSetId::new("changeset-operation-task")?,
        title: "Two-file implementation".to_owned(),
        summary: "Update retry and telemetry.".to_owned(),
        risk: ChangeSetRisk::Low,
        files: files
            .iter()
            .map(|(path, before, after)| ChangeSetFile {
                path: (*path).to_owned(),
                previous_path: None,
                action: ChangeSetFileAction::Update,
                risk: ChangeSetRisk::Low,
                before_hash: Some(IntentContentDigest::from_bytes(before).as_str().to_owned()),
                after_hash: Some(IntentContentDigest::from_bytes(after).as_str().to_owned()),
                diff_hash: Some(format!("sha256:{}", "c".repeat(64))),
                additions: 1,
                deletions: 1,
                validations: Vec::new(),
            })
            .collect(),
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
        "artifact:changeset-operation-task",
        Vec::new(),
    )?;
    let proposal = IntegrationProposalSpec::from_changeset(
        &change_set,
        step_id,
        "snapshot-operation-task-base".to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        "scope-operation-task",
        proposal_facts,
    )?;
    let integration_plan = build_integration_plan(
        IntegrationPlanId::new("integration-operation-task")?,
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
    let batch_id = "batch-operation-task".to_owned();
    let subjects = files
        .iter()
        .map(|(path, _, _)| MutationSubject::File {
            path: (*path).into(),
            file_type: FileType::File,
        })
        .collect::<Vec<_>>();
    recorder.append_batch_started(&batch_id, "operation-task-source-batch", &subjects)?;
    let mut committed_ids = Vec::new();
    for (index, (path, before, after)) in files.iter().enumerate() {
        let operation_id = format!("operation-task-source-{index}");
        let before_hash = IntentContentDigest::from_bytes(before);
        let after_hash = IntentContentDigest::from_bytes(after);
        let before_artifact = recorder.capture_immutable_content_artifact(
            &workspace_id,
            &operation_id,
            Path::new(path),
            before,
        )?;
        recorder.append_prepared(&MutationPrepared {
            operation_id: operation_id.clone(),
            batch_id: Some(batch_id.clone()),
            tool_call_id: Some("tool-operation-task".to_owned()),
            causation_event_id: "event-operation-task".to_owned(),
            subject: subjects[index].clone(),
            before_hash: Some(before_hash.as_str().to_owned()),
            intended_after_hash: Some(after_hash.as_str().to_owned()),
            snapshot_coverage: SnapshotCoverage::Captured(before_artifact),
            workspace_id: workspace_id.clone(),
            base_workspace_revision: u64::try_from(index).unwrap_or_default(),
            sync_class: MutationSyncClass::RecoveryCritical,
        })?;
        fs::write(workspace.join(path), after)?;
        recorder.append_committed(&MutationCommitted {
            operation_id: operation_id.clone(),
            batch_id: Some(batch_id.clone()),
            workspace_id: Some(workspace_id.clone()),
            observed_after_hash: Some(after_hash.as_str().to_owned()),
            workspace_revision: u64::try_from(index).unwrap_or_default() + 1,
            workspace_snapshot_id: format!("snapshot-operation-task-file-{index}"),
            committed_subject: subjects[index].clone(),
        })?;
        committed_ids.push(operation_id);
    }
    recorder.append_batch_finished(&batch_id, MutationBatchStatus::Applied, &committed_ids, &[])?;
    session.append_control(ControlEntry::ChangeSetApplied(ChangeSetResult {
        id: change_set.id,
        status: ChangeSetResultStatus::Applied,
        file_results: files
            .iter()
            .map(|(path, _, _)| ChangeSetFileResult {
                path: (*path).to_owned(),
                action: ChangeSetFileAction::Update,
                status: ChangeSetFileResultStatus::Applied,
                message: None,
                validations: Vec::new(),
            })
            .collect(),
        message: None,
    }))?;
    session.append_control(ControlEntry::IntegrationPromotionRecorded(
        IntegrationPromotionRecorded {
            plan_id: integration_plan.plan_id,
            attempt_id: None,
            status: IntegrationPromotionStatus::Promoted,
            preview_digest: format!("sha256:{}", "d".repeat(64)),
            target: IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id: "snapshot-operation-task-base".to_owned(),
                expected_revision: 0,
            },
            authority_nonce: None,
            effect: Some(IntegrationPromotionEffect::WorkspaceApplied {
                promoted_snapshot_id: "snapshot-operation-task-promoted".to_owned(),
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
    materialize_intent_layer(
        &session,
        &workspace,
        &execution_id,
        &SecretRedactor::empty(),
    )?;
    Ok((store, session, intent_ref))
}

fn authority() -> Result<IntentOperationAuthorityV1> {
    IntentOperationAuthorityV1::new(
        IntentDigest::new(format!(
            "{}{}",
            INTENT_CANONICAL_DIGEST_PREFIX,
            "a".repeat(64)
        ))?,
        "approval-intent-drop",
        None,
    )
}

fn request(preview: &IntentOperationPreviewV1) -> IntentDropRequestV1 {
    IntentDropRequestV1 {
        operation_id: preview.operation_id.clone(),
        stack_version: preview.stack_version,
        preview_digest: preview.preview_digest.clone(),
    }
}

#[test]
fn exact_drop_applies_reverse_bytes_and_projects_dropped() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    assert!(preview.conflicts.is_empty());
    assert!(preview.target_is_leaf);
    assert_eq!(preview.file_effects.len(), 1);
    assert_eq!(
        preview.file_effects[0].action,
        IntentOperationFileAction::Update
    );
    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { stack, .. }
            if stack.intents[0].available_actions == vec![IntentOperationKind::Drop]
    ));

    let output = execute_intent_drop(
        &session,
        &workspace,
        &request(&preview),
        &authority()?,
        "remove the retry update",
    )?;
    assert_eq!(output.resolution, IntentOperationResolution::Committed);
    assert_eq!(
        fs::read_to_string(workspace.join("src/retry.rs"))?,
        "fn retry() { 1 }\n"
    );
    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { stack, .. }
            if stack.intents[0].application_state == IntentApplicationState::Dropped
                && stack.intents[0].available_actions.is_empty()
                && stack.intents[0].system_verified_criterion_count == 0
    ));
    let projection = session.intent_operation_projection()?;
    assert_eq!(
        projection
            .operation(&preview.operation_id)
            .context("operation is projected")?
            .state,
        IntentOperationStateV1::Committed
    );
    Ok(())
}

#[test]
fn exact_drop_preserves_create_delete_presence_semantics() -> Result<()> {
    let created = tempdir()?;
    let (_store, created_session, created_intent, _) =
        setup_chat_drop_contents(created.path(), None, Some(b"new file\n"))?;
    let created_workspace = created.path().join("workspace");
    let created_preview =
        preview_intent_drop(&created_session, &created_workspace, &created_intent)?;
    assert_eq!(
        created_preview.file_effects[0].action,
        IntentOperationFileAction::Delete
    );
    execute_intent_drop(
        &created_session,
        &created_workspace,
        &request(&created_preview),
        &authority()?,
        "remove created file",
    )?;
    assert!(!created_workspace.join("src/retry.rs").exists());

    let deleted = tempdir()?;
    let (_store, deleted_session, deleted_intent, _) =
        setup_chat_drop_contents(deleted.path(), Some(b"old file\n"), None)?;
    let deleted_workspace = deleted.path().join("workspace");
    let deleted_preview =
        preview_intent_drop(&deleted_session, &deleted_workspace, &deleted_intent)?;
    assert_eq!(
        deleted_preview.file_effects[0].action,
        IntentOperationFileAction::Create
    );
    execute_intent_drop(
        &deleted_session,
        &deleted_workspace,
        &request(&deleted_preview),
        &authority()?,
        "restore deleted file",
    )?;
    assert_eq!(
        fs::read_to_string(deleted_workspace.join("src/retry.rs"))?,
        "old file\n"
    );
    Ok(())
}

#[test]
fn stale_preview_fails_before_request_or_write() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    fs::write(workspace.join("src/retry.rs"), "human edit\n")?;

    let error = execute_intent_drop(
        &session,
        &workspace,
        &request(&preview),
        &authority()?,
        "remove stale change",
    )
    .expect_err("stale preview must fail");
    assert!(error.to_string().contains("preview digest is stale"));
    assert_eq!(
        fs::read_to_string(workspace.join("src/retry.rs"))?,
        "human edit\n"
    );
    assert!(
        !JsonlSessionStore::read_event_records(store.path())?
            .iter()
            .any(|record| {
                record.stored_event().event_kind()
                    == Some(DurableEventType::IntentOperationRequested)
            })
    );
    Ok(())
}

#[test]
fn shared_or_unavailable_layer_returns_typed_conflict_without_request() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let lineage = IntentLineageProjectionV1::from_records(&records, &admission)?;
    let layers = IntentLayerProjectionV1::from_records(&records, &admission, &lineage)?;
    let operations = IntentOperationProjectionV1::from_records(&records, &admission, &layers)?;
    let recorder = session.mutation_event_recorder().context("recorder")?;

    let mut shared = layers.clone();
    shared
        .layers
        .values_mut()
        .next()
        .context("layer")?
        .artifacts[0]
        .ownership = IntentArtifactOwnership::Shared;
    let shared_preview = resolve_drop_from_parts(
        &records,
        &workspace,
        &recorder,
        &admission,
        &lineage,
        &shared,
        &operations,
        &intent_ref,
    )?
    .preview;
    assert!(
        shared_preview
            .conflicts
            .iter()
            .any(|conflict| { conflict.code == IntentOperationErrorCode::SharedArtifact })
    );

    let mut unavailable = layers;
    unavailable
        .layers
        .values_mut()
        .next()
        .context("layer")?
        .artifacts[0]
        .availability = IntentArtifactAvailability::Deleted;
    let unavailable_preview = resolve_drop_from_parts(
        &records,
        &workspace,
        &recorder,
        &admission,
        &lineage,
        &unavailable,
        &operations,
        &intent_ref,
    )?
    .preview;
    assert!(
        unavailable_preview
            .conflicts
            .iter()
            .any(|conflict| { conflict.code == IntentOperationErrorCode::ArtifactUnavailable })
    );
    assert!(
        !JsonlSessionStore::read_event_records(store.path())?
            .iter()
            .any(|record| {
                record.stored_event().event_kind()
                    == Some(DurableEventType::IntentOperationRequested)
            })
    );
    Ok(())
}

#[test]
fn dependency_target_is_not_leaf_and_cannot_be_forged_by_renderer() -> Result<()> {
    let base_id = IntentId::new("intent-base")?;
    let dependent_id = IntentId::new("intent-dependent")?;
    let base_ref = IntentVersionRef::new(base_id.clone(), 1)?;
    let dependent_ref = IntentVersionRef::new(dependent_id.clone(), 1)?;
    let criterion = |id: &str| -> Result<IntentAcceptanceCriterionV1> {
        Ok(IntentAcceptanceCriterionV1 {
            criterion_id: IntentCriterionId::new(id)?,
            statement: "criterion".to_owned(),
            required: true,
        })
    };
    let mut plan = IntentPlanV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        stack_id: IntentStackId::new("stack-leaf-test")?,
        stack_version: IntentStackVersion::new(1)?,
        workspace_id: "workspace-leaf-test".to_owned(),
        source_session_id: "session-leaf-test".to_owned(),
        kind: IntentPlanKind::SuggestedDecomposition,
        intents: vec![
            IntentDefinitionV1 {
                intent_ref: base_ref.clone(),
                title: "Base".to_owned(),
                statement: "Base statement".to_owned(),
                acceptance_criteria: vec![criterion("criterion-base")?],
                depends_on: Vec::new(),
                source: IntentSourceV1::UserTurn {
                    source_turn_id: "turn-leaf".to_owned(),
                },
                supersedes: None,
            },
            IntentDefinitionV1 {
                intent_ref: dependent_ref.clone(),
                title: "Dependent".to_owned(),
                statement: "Dependent statement".to_owned(),
                acceptance_criteria: vec![criterion("criterion-dependent")?],
                depends_on: vec![base_id],
                source: IntentSourceV1::UserTurn {
                    source_turn_id: "turn-leaf".to_owned(),
                },
                supersedes: None,
            },
        ],
        plan_digest: zero_intent_digest()?,
    };
    plan.plan_digest = plan.computed_digest()?;
    let accepted = AcceptedIntentPlanProjectionV1 {
        plan,
        acceptance_kind: IntentAcceptanceKind::ExplicitUserConfirmation,
        source_turn_id: "turn-leaf".to_owned(),
        acceptance_authority_id: "authority-leaf".to_owned(),
        task_plan_binding: None,
        accepted_event_id: "event-leaf".to_owned(),
        accepted_stream_sequence: 1,
    };
    let operations = IntentOperationProjectionV1::default();
    let layers = IntentLayerProjectionV1::default();
    let operation_id = operation_id_for_preview_target(
        &accepted,
        &layers,
        &operations,
        &accepted.plan.intents[0],
        0,
        1,
    )
    .context("operation id")?;
    let mut preview = IntentOperationPreviewV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        operation_id,
        operation_kind: IntentOperationKind::Drop,
        stack_id: accepted.plan.stack_id.clone(),
        stack_version: accepted.plan.stack_version,
        target_intents: vec![base_ref.clone()],
        target_is_leaf: false,
        workspace_revision: 0,
        expires_at_ms: None,
        file_effects: Vec::new(),
        retained_intents: vec![dependent_ref],
        verification_impacts: Vec::new(),
        conflicts: vec![intent_conflict(
            IntentOperationErrorCode::TargetNotLeaf,
            Some(base_ref),
            None,
            "Active downstream intent depends on this target",
        )],
        preview_digest: zero_intent_digest()?,
    };
    preview.preview_digest = preview.computed_digest()?;
    validate_requested_preview(&preview, &accepted, &layers, &BTreeSet::new(), 2)?;

    preview.target_is_leaf = true;
    preview.preview_digest = preview.computed_digest()?;
    assert!(
        validate_requested_preview(&preview, &accepted, &layers, &BTreeSet::new(), 2)
            .expect_err("renderer cannot forge leaf status")
            .to_string()
            .contains("dependency")
    );
    Ok(())
}

#[test]
fn fully_applied_batch_without_terminal_is_repaired_without_replay() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    let resolved = resolve_drop(&session, &workspace, &intent_ref)?;
    let authority = authority()?;
    let batch_id = intent_drop_batch_id(&preview.operation_id);
    append_operation_requested(
        &store,
        &preview,
        "repair terminal",
        resolved.source_frontier_sequence,
    )?;
    append_operation_prepared(&store, &resolved, &authority, &batch_id)?;
    let recorder = session.mutation_event_recorder().context("recorder")?;
    let coordinator = recorder.coordinator_with_workspace_lease(
        &workspace,
        preview.operation_id.as_str(),
        Some(batch_id.clone()),
    )?;
    let subjects = resolved
        .files
        .iter()
        .map(|file| MutationSubject::File {
            path: PathBuf::from(&file.path),
            file_type: FileType::File,
        })
        .collect::<Vec<_>>();
    recorder.append_bound_batch_started(
        &batch_id,
        preview.operation_id.as_str(),
        &subjects,
        Some(preview.preview_digest.as_str()),
        Some(&authority.approval_authority_id),
        Some(authority.permission_policy_digest.as_str()),
    )?;
    let file = &resolved.files[0];
    let prepared = coordinator.prepare_file_expected(
        PathBuf::from(&file.path),
        file.absolute_path.clone(),
        file.expected_hash.clone(),
        file.target.as_deref().map(bytes_hash),
    )?;
    let committed = coordinator.commit_write(
        &prepared,
        file.target.as_deref().context("update has target bytes")?,
    )?;
    let committed_operation_id = committed.operation_id;
    drop(coordinator);

    let repaired = reconcile_intent_operations(&mut session, &workspace)?;
    assert_eq!(repaired, vec![preview.operation_id.clone()]);
    assert_eq!(
        fs::read_to_string(workspace.join("src/retry.rs"))?,
        "fn retry() { 1 }\n"
    );
    assert_eq!(
        session
            .intent_operation_projection()?
            .operation(&preview.operation_id)
            .context("repaired operation")?
            .state,
        IntentOperationStateV1::Committed
    );
    assert_eq!(
        JsonlSessionStore::read_event_records(store.path())?
            .iter()
            .filter(|record| {
                record.stored_event().event_kind()
                    == Some(DurableEventType::IntentOperationResolved)
            })
            .count(),
        1
    );
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(records.iter().any(|record| {
        if record.stored_event().event_kind() != Some(DurableEventType::MutationBatchFinished) {
            return false;
        }
        serde_json::from_value::<MutationBatchFinished>(record.stored_event().payload.clone())
            .is_ok_and(|terminal| {
                terminal.status == MutationBatchStatus::Applied
                    && terminal.committed_operations == vec![committed_operation_id.clone()]
            })
    }));
    Ok(())
}

#[test]
fn prepared_file_is_reconciled_interrupted_without_replaying_reverse_write() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let resolved = resolve_drop(&session, &workspace, &intent_ref)?;
    let preview = resolved.preview.clone();
    let authority = authority()?;
    let batch_id = intent_drop_batch_id(&preview.operation_id);
    append_operation_requested(
        &store,
        &preview,
        "do not replay",
        resolved.source_frontier_sequence,
    )?;
    append_operation_prepared(&store, &resolved, &authority, &batch_id)?;
    let recorder = session.mutation_event_recorder().context("recorder")?;
    let coordinator = recorder.coordinator_with_workspace_lease(
        &workspace,
        preview.operation_id.as_str(),
        Some(batch_id.clone()),
    )?;
    let file = &resolved.files[0];
    recorder.append_bound_batch_started(
        &batch_id,
        preview.operation_id.as_str(),
        &[MutationSubject::File {
            path: PathBuf::from(&file.path),
            file_type: FileType::File,
        }],
        Some(preview.preview_digest.as_str()),
        Some(&authority.approval_authority_id),
        Some(authority.permission_policy_digest.as_str()),
    )?;
    coordinator.prepare_file_expected(
        PathBuf::from(&file.path),
        file.absolute_path.clone(),
        file.expected_hash.clone(),
        file.target.as_deref().map(bytes_hash),
    )?;
    drop(coordinator);

    reconcile_intent_operations(&mut session, &workspace)?;
    assert_eq!(
        fs::read_to_string(workspace.join("src/retry.rs"))?,
        "fn retry() { 2 }\n"
    );
    assert_eq!(
        session
            .intent_operation_projection()?
            .operation(&preview.operation_id)
            .context("interrupted operation")?
            .state,
        IntentOperationStateV1::Interrupted
    );
    Ok(())
}

#[test]
fn all_drop_files_preflight_before_the_first_workspace_write() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref) = setup_task_multifile_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let resolved = resolve_drop(&session, &workspace, &intent_ref)?;
    assert_eq!(resolved.files.len(), 2);
    let recorder = session.mutation_event_recorder().context("recorder")?;
    let coordinator = recorder.coordinator_with_workspace_lease(
        &workspace,
        resolved.preview.operation_id.as_str(),
        Some("intent-drop-preflight-test".to_owned()),
    )?;

    fs::write(workspace.join("src/telemetry.rs"), b"external drift\n")?;
    let (_, prepared_operation_ids) =
        prepare_drop_files(&coordinator, &resolved.files).expect_err("second preflight must fail");

    assert_eq!(prepared_operation_ids.len(), 1);
    assert_eq!(
        fs::read(workspace.join("src/retry.rs"))?,
        b"fn retry() { 2 }\n"
    );
    assert_eq!(
        fs::read(workspace.join("src/telemetry.rs"))?,
        b"external drift\n"
    );
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(!records.iter().any(|record| {
        record.stored_event().event_kind() == Some(DurableEventType::MutationCommitted)
            && record
                .stored_event()
                .payload
                .get("batch_id")
                .and_then(serde_json::Value::as_str)
                == Some("intent-drop-preflight-test")
    }));
    Ok(())
}

#[test]
fn partial_multifile_batch_never_projects_target_as_dropped() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref) = setup_task_multifile_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let resolved = resolve_drop(&session, &workspace, &intent_ref)?;
    assert_eq!(resolved.files.len(), 2);
    let preview = resolved.preview.clone();
    let authority = authority()?;
    let batch_id = intent_drop_batch_id(&preview.operation_id);
    append_operation_requested(
        &store,
        &preview,
        "simulate partial apply",
        resolved.source_frontier_sequence,
    )?;
    append_operation_prepared(&store, &resolved, &authority, &batch_id)?;
    let recorder = session.mutation_event_recorder().context("recorder")?;
    let coordinator = recorder.coordinator_with_workspace_lease(
        &workspace,
        preview.operation_id.as_str(),
        Some(batch_id.clone()),
    )?;
    let subjects = resolved
        .files
        .iter()
        .map(|file| MutationSubject::File {
            path: PathBuf::from(&file.path),
            file_type: FileType::File,
        })
        .collect::<Vec<_>>();
    recorder.append_bound_batch_started(
        &batch_id,
        preview.operation_id.as_str(),
        &subjects,
        Some(preview.preview_digest.as_str()),
        Some(&authority.approval_authority_id),
        Some(authority.permission_policy_digest.as_str()),
    )?;
    let first = &resolved.files[0];
    let first_prepared = coordinator.prepare_file_expected(
        PathBuf::from(&first.path),
        first.absolute_path.clone(),
        first.expected_hash.clone(),
        first.target.as_deref().map(bytes_hash),
    )?;
    let first_committed = coordinator.commit_write(
        &first_prepared,
        first.target.as_deref().context("first target")?,
    )?;
    let second = &resolved.files[1];
    let second_prepared = coordinator.prepare_file_expected(
        PathBuf::from(&second.path),
        second.absolute_path.clone(),
        second.expected_hash.clone(),
        second.target.as_deref().map(bytes_hash),
    )?;
    recorder.append_bound_batch_finished(
        &batch_id,
        MutationBatchStatus::PartiallyApplied,
        std::slice::from_ref(&first_committed.operation_id),
        std::slice::from_ref(&second_prepared.operation_id),
        &[],
        &[],
        Some(preview.preview_digest.as_str()),
        Some(&authority.approval_authority_id),
        Some(authority.permission_policy_digest.as_str()),
    )?;
    append_operation_resolved(
        &store,
        &preview.operation_id,
        IntentOperationResolution::PartiallyApplied,
        Some(&batch_id),
        Some(&first_committed.workspace_snapshot_id),
        Some(IntentOperationErrorCode::PartialApplication),
    )?;
    drop(coordinator);

    assert_eq!(
        session
            .intent_operation_projection()?
            .operation(&preview.operation_id)
            .context("partial operation")?
            .state,
        IntentOperationStateV1::PartiallyApplied
    );
    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { stack, .. }
            if stack.intents[0].application_state != IntentApplicationState::Dropped
    ));
    assert_eq!(
        fs::read_to_string(workspace.join("src/retry.rs"))?,
        "fn retry() { 1 }\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("src/telemetry.rs"))?,
        "fn telemetry() { 2 }\n"
    );
    Ok(())
}

#[test]
fn checkpoint_restore_conflicts_with_active_intent_and_ignores_drop_mutations() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let records =
        JsonlSessionStore::read_event_records(session.store_path().context("store path")?)?;
    let checkpoints = ControlledCheckpointProjection::from_records(&records)?;
    let checkpoint = checkpoints.latest().context("source checkpoint")?;
    let checkpoint_request = ControlledCheckpointRestoreRequest {
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        checkpoint_digest: checkpoint.checkpoint_digest.clone(),
    };
    let recorder = session.mutation_event_recorder().context("recorder")?;
    let preview = preview_controlled_checkpoint_restore(
        &recorder,
        &records,
        &workspace,
        &checkpoint_request,
    )?;
    assert!(preview.files.iter().any(|file| {
        file.conflict_reason == Some(CheckpointRestoreConflictReason::IntentStateConflict)
    }));

    let drop_preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    execute_intent_drop(
        &session,
        &workspace,
        &request(&drop_preview),
        &authority()?,
        "drop before checkpoint replay",
    )?;
    let after = JsonlSessionStore::read_event_records(session.store_path().context("store path")?)?;
    let checkpoints_after = ControlledCheckpointProjection::from_records(&after)?;
    assert_eq!(
        checkpoints_after
            .latest()
            .context("checkpoint remains")?
            .files
            .len(),
        checkpoint.files.len()
    );
    Ok(())
}

#[test]
fn cancelled_prepared_operation_has_no_workspace_effect() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let resolved = resolve_drop(&session, &workspace, &intent_ref)?;
    let preview = resolved.preview.clone();
    append_operation_requested(
        &store,
        &preview,
        "cancel me",
        resolved.source_frontier_sequence,
    )?;
    append_operation_prepared(
        &store,
        &resolved,
        &authority()?,
        &intent_drop_batch_id(&preview.operation_id),
    )?;
    assert!(cancel_intent_operation(&session, &preview.operation_id)?);
    assert!(!cancel_intent_operation(&session, &preview.operation_id)?);
    assert_eq!(
        fs::read_to_string(workspace.join("src/retry.rs"))?,
        "fn retry() { 2 }\n"
    );
    assert_eq!(
        session
            .intent_operation_projection()?
            .operation(&preview.operation_id)
            .context("cancelled operation")?
            .state,
        IntentOperationStateV1::Cancelled
    );
    Ok(())
}

#[test]
fn expired_host_authority_is_durably_rejected_before_prepare() -> Result<()> {
    let temp = tempdir()?;
    let (store, session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    let expired = IntentOperationAuthorityV1::new(
        IntentDigest::new(format!(
            "{}{}",
            INTENT_CANONICAL_DIGEST_PREFIX,
            "b".repeat(64)
        ))?,
        "expired-intent-approval",
        Some(1),
    )?;
    let output = execute_intent_drop(
        &session,
        &workspace,
        &request(&preview),
        &expired,
        "expired approval",
    )?;
    assert_eq!(output.resolution, IntentOperationResolution::Rejected);
    assert_eq!(
        fs::read_to_string(workspace.join("src/retry.rs"))?,
        "fn retry() { 2 }\n"
    );
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(records.iter().any(|record| {
        record.stored_event().event_kind() == Some(DurableEventType::IntentOperationRequested)
    }));
    assert!(!records.iter().any(|record| {
        record.stored_event().event_kind() == Some(DurableEventType::IntentOperationPrepared)
    }));

    let retry_preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    assert_ne!(retry_preview.operation_id, preview.operation_id);
    let retry = execute_intent_drop(
        &session,
        &workspace,
        &request(&retry_preview),
        &authority()?,
        "retry with current approval",
    )?;
    assert_eq!(retry.resolution, IntentOperationResolution::Committed);
    Ok(())
}

#[test]
fn dropped_layer_exits_retention_protected_set_but_history_stays_dropped() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");
    let preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    execute_intent_drop(
        &session,
        &workspace,
        &request(&preview),
        &authority()?,
        "drop before retention",
    )?;
    let recorder = session.mutation_event_recorder().context("recorder")?;
    let report = recorder.enforce_artifact_retention_at(
        &MutationArtifactRetentionPolicy {
            max_artifacts: Some(0),
            max_bytes: Some(0),
            expire_older_than_ms: Some(u64::MAX),
        },
        u64::MAX,
    )?;
    assert!(report.deleted_artifacts + report.expired_artifacts >= 4);
    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { stack, .. }
            if stack.intents[0].application_state == IntentApplicationState::Dropped
    ));
    Ok(())
}

#[test]
fn superseded_layer_and_historical_operation_replay_without_retention_authority() -> Result<()> {
    let temp = tempdir()?;
    let (_store, mut session, intent_ref, _execution_id) = setup_chat_drop(temp.path())?;
    let workspace = temp.path().join("workspace");

    let drop_preview = preview_intent_drop(&session, &workspace, &intent_ref)?;
    let expired = IntentOperationAuthorityV1::new(
        IntentDigest::new(format!(
            "{}{}",
            INTENT_CANONICAL_DIGEST_PREFIX,
            "c".repeat(64)
        ))?,
        "expired-before-revision",
        Some(1),
    )?;
    assert_eq!(
        execute_intent_drop(
            &session,
            &workspace,
            &request(&drop_preview),
            &expired,
            "retain rejected historical operation",
        )?
        .resolution,
        IntentOperationResolution::Rejected
    );

    let revision = IntentRevisionProposalV1::new(
        "revision-after-layer",
        "turn-revision-after-layer",
        intent_ref,
        "Revised retry",
        "Rebuild retry with the revised behavior.",
        vec![IntentProposalCriterionV1 {
            criterion_alias: "revised-retry-check".to_owned(),
            statement: "The revised retry behavior is verified.".to_owned(),
            required: true,
        }],
        Vec::new(),
    )?;
    let impact = preview_intent_revision(&session, &workspace, &revision)?;
    let revision_authority = IntentRevisionAuthorityV1::explicit_user_confirmation(
        &revision,
        &impact,
        "decision-revision-after-layer",
    )?;
    accept_intent_revision(
        &mut session,
        &workspace,
        &revision,
        &impact,
        &revision_authority,
        None,
    )?;

    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { stack, .. }
            if stack.stack_version.get() == 2
                && stack.intents[0].application_state == IntentApplicationState::NeedsRebuild
    ));
    assert_eq!(
        session
            .intent_operation_projection()?
            .operations
            .get(&drop_preview.operation_id)
            .map(|operation| operation.state),
        Some(IntentOperationStateV1::Rejected),
        "historical operation stays auditable under its original plan version"
    );

    let recorder = session.mutation_event_recorder().context("recorder")?;
    let report = recorder.enforce_artifact_retention_at(
        &MutationArtifactRetentionPolicy {
            max_artifacts: Some(0),
            max_bytes: Some(0),
            expire_older_than_ms: Some(u64::MAX),
        },
        u64::MAX,
    )?;
    assert!(
        report.deleted_artifacts + report.expired_artifacts >= 4,
        "superseded layer artifacts must leave the active retention-protected set"
    );
    Ok(())
}
