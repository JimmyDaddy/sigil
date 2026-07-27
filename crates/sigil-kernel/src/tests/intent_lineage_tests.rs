use anyhow::{Context, Result};
use tempfile::tempdir;

use super::*;
use crate::*;

fn session_at(path: &std::path::Path) -> Result<(JsonlSessionStore, Session)> {
    let store = JsonlSessionStore::new(path)?;
    let session = Session::load_from_store("test-provider", "test-model", store.clone())?;
    Ok((store, session))
}

fn root_admission(session: &Session, stack_id: &str) -> Result<IntentPlanAdmissionV1> {
    let context = IntentAdmissionContextV1::initial(
        IntentStackId::new(stack_id)?,
        "workspace-demo",
        session.session_scope_id(),
    )?;
    let authority =
        IntentAcceptanceAuthorityV1::user_declared_root("turn-root", "event-user-root")?;
    admit_user_declared_root(
        &context,
        UserDeclaredIntentV1 {
            title: "Update retry behavior".to_owned(),
            statement: "Update the bounded retry behavior.".to_owned(),
            acceptance_criteria: vec![IntentProposalCriterionV1 {
                criterion_alias: "retry-check".to_owned(),
                statement: "The scoped retry check passes.".to_owned(),
                required: true,
            }],
        },
        &authority,
    )
}

fn start_chat_execution(
    session: &mut Session,
    stack_id: &str,
) -> Result<(IntentVersionRef, IntentCriterionId, IntentExecutionId)> {
    let admission = root_admission(session, stack_id)?;
    append_chat_root_intent_admission(session, &admission)?;
    let intent = &admission.plan().intents[0];
    let intent_ref = intent.intent_ref.clone();
    let criterion_id = intent.acceptance_criteria[0].criterion_id.clone();
    session.append_control(ControlEntry::AgentRunAttemptStarted(
        AgentRunAttemptStartedEntry {
            thread_id: AgentThreadId::new("root-run-1")?,
            attempt_id: AgentRunAttemptId::new("chat-attempt-1")?,
            provider: "test-provider".to_owned(),
            model: "test-model".to_owned(),
            background: false,
            provider_background_handle_ref: None,
        },
    ))?;
    let outcome = append_chat_intent_execution_binding(
        session,
        intent_ref.clone(),
        "root-run-1",
        "turn-root",
        "chat-attempt-1",
    )?;
    Ok((
        intent_ref,
        criterion_id,
        outcome.execution_id.context("execution id is present")?,
    ))
}

fn append_chat_mutation(
    store: &JsonlSessionStore,
    session: &mut Session,
    execution_id: &IntentExecutionId,
    operation_id: &str,
    snapshot_id: &str,
) -> Result<ChangeSetId> {
    let recorder = MutationEventRecorder::new(store.clone());
    let prepared = recorder.append_prepared(&MutationPrepared {
        operation_id: operation_id.to_owned(),
        batch_id: None,
        tool_call_id: Some("tool-call-write".to_owned()),
        causation_event_id: "event-tool-start".to_owned(),
        subject: MutationSubject::File {
            path: "src/retry.rs".into(),
            file_type: FileType::File,
        },
        before_hash: Some(format!("sha256:{}", "a".repeat(64))),
        intended_after_hash: Some(format!("sha256:{}", "b".repeat(64))),
        snapshot_coverage: SnapshotCoverage::Captured("artifact-before".to_owned()),
        workspace_id: "workspace-demo".to_owned(),
        base_workspace_revision: 0,
        sync_class: MutationSyncClass::RecoveryCritical,
    })?;
    let committed = recorder.append_committed(&MutationCommitted {
        operation_id: operation_id.to_owned(),
        batch_id: None,
        workspace_id: Some("workspace-demo".to_owned()),
        observed_after_hash: Some(format!("sha256:{}", "b".repeat(64))),
        workspace_revision: 1,
        workspace_snapshot_id: snapshot_id.to_owned(),
        committed_subject: MutationSubject::File {
            path: "src/retry.rs".into(),
            file_type: FileType::File,
        },
    })?;
    append_chat_direct_mutation_changeset_binding(
        session,
        execution_id,
        &prepared.event_id,
        &committed.event_id,
    )?;
    let projection = session.intent_lineage_projection()?;
    Ok(projection
        .execution(execution_id)
        .context("execution is projected")?
        .changeset_ids[0]
        .clone())
}

fn sample_changeset(id: &str) -> Result<ChangeSet> {
    Ok(ChangeSet {
        id: ChangeSetId::new(id)?,
        title: "Retry implementation".to_owned(),
        summary: "Update retry behavior.".to_owned(),
        risk: ChangeSetRisk::Low,
        files: vec![ChangeSetFile {
            path: "src/retry.rs".to_owned(),
            previous_path: None,
            action: ChangeSetFileAction::Update,
            risk: ChangeSetRisk::Low,
            before_hash: Some(format!("sha256:{}", "a".repeat(64))),
            after_hash: Some(format!("sha256:{}", "b".repeat(64))),
            diff_hash: Some(format!("sha256:{}", "c".repeat(64))),
            additions: 1,
            deletions: 1,
            validations: Vec::new(),
        }],
        validations: Vec::new(),
    })
}

#[test]
fn task_plan_aliases_resolve_to_stable_refs_and_unknown_aliases_fail_closed() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session) = session_at(&temp.path().join("session.jsonl"))?;
    let admission = root_admission(&session, "stack-task-alias")?;
    let step_id = TaskStepId::new("write-retry")?;
    let task_plan = TaskPlanEntry {
        task_id: TaskId::new("task-alias")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "Write retry".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Write),
            isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
        }],
        reason: None,
    };
    let bound = bind_task_plan_intents(
        &admission,
        task_plan.clone(),
        &[TaskStepIntentAliasBindingV1 {
            step_id: step_id.clone(),
            intent_aliases: vec![USER_DECLARED_ROOT_INTENT_ALIAS.to_owned()],
        }],
    )?;
    assert_eq!(
        bound.steps[0].intent_refs,
        vec![admission.plan().intents[0].intent_ref.clone()]
    );
    assert!(
        bind_task_plan_intents(
            &admission,
            task_plan,
            &[TaskStepIntentAliasBindingV1 {
                step_id,
                intent_aliases: vec!["model-invented-id".to_owned()],
            }],
        )
        .expect_err("unknown provider alias must fail")
        .to_string()
        .contains("unknown intent alias")
    );
    Ok(())
}

#[test]
fn intent_scoped_check_hash_is_order_independent_and_rejects_duplicate_scope() -> Result<()> {
    let first_ref = IntentVersionRef::new(IntentId::new("intent-first")?, 1)?;
    let second_ref = IntentVersionRef::new(IntentId::new("intent-second")?, 1)?;
    let first_criterion = IntentCriterionId::new("criterion-first")?;
    let second_criterion = IntentCriterionId::new("criterion-second")?;
    let base = CheckSpec::new(
        "check-scoped",
        CheckCommand::shell("cargo test"),
        ToolEffect::ReadOnly,
        "scope-main",
    );
    let forward = base
        .clone()
        .with_intent_scope(first_ref.clone(), vec![first_criterion.clone()])?
        .with_intent_scope(second_ref.clone(), vec![second_criterion.clone()])?;
    let reverse = base
        .with_intent_scope(second_ref, vec![second_criterion])?
        .with_intent_scope(first_ref.clone(), vec![first_criterion.clone()])?;
    assert_eq!(forward.check_spec_hash, reverse.check_spec_hash);
    forward.validate_shape()?;
    let mut tampered = forward.clone();
    tampered.intent_scopes[0].criterion_ids[0] = IntentCriterionId::new("criterion-tampered")?;
    assert!(
        tampered
            .validate_shape()
            .expect_err("persisted scope mutation must invalidate the check hash")
            .to_string()
            .contains("hash")
    );
    assert!(
        forward
            .with_intent_scope(first_ref, vec![first_criterion])
            .expect_err("duplicate intent scope must fail")
            .to_string()
            .contains("repeats")
    );
    Ok(())
}

#[test]
fn chat_direct_file_mutation_materializes_bounded_changeset_and_parent_lineage() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session) = session_at(&temp.path().join("session.jsonl"))?;
    let (intent_ref, _criterion_id, execution_id) =
        start_chat_execution(&mut session, "stack-chat-direct")?;
    let change_set_id = append_chat_mutation(
        &store,
        &mut session,
        &execution_id,
        "operation-chat-1",
        "snapshot-chat-1",
    )?;

    let projection = session.intent_lineage_projection()?;
    let execution = projection
        .execution(&execution_id)
        .context("execution exists")?;
    assert_eq!(
        execution.parent_snapshot_id.as_deref(),
        Some("snapshot-chat-1")
    );
    assert!(execution.parent_mutation_event_id.is_some());
    assert_eq!(execution.changeset_ids, vec![change_set_id.clone()]);
    let summary = projection.summary_for(&intent_ref);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::NeedsReview)
    );
    assert_eq!(summary.read_only_reason, None);

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let facts = DurableLineageFacts::from_records(&records)?;
    let projected = &facts
        .changesets
        .get(&change_set_id)
        .context("bounded ChangeSet is durable")?
        .value;
    assert_eq!(projected.files.len(), 1);
    assert_eq!(projected.files[0].path, "src/retry.rs");
    assert_eq!(projected.files[0].action, ChangeSetFileAction::Update);
    Ok(())
}

#[test]
fn lineage_gap_degrades_to_read_only_instead_of_guessing_parent_mutation() -> Result<()> {
    let temp = tempdir()?;
    let (_store, mut session) = session_at(&temp.path().join("session.jsonl"))?;
    let (intent_ref, _criterion_id, execution_id) =
        start_chat_execution(&mut session, "stack-chat-read-only")?;
    let change_set = sample_changeset("changeset-no-parent")?;
    session.append_control(ControlEntry::ChangeSetProposed(change_set.clone()))?;
    append_intent_changeset_binding(&session, &execution_id, vec![change_set.id])?;

    let summary = session
        .intent_lineage_projection()?
        .summary_for(&intent_ref);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::ReadOnly)
    );
    assert_eq!(
        summary.read_only_reason,
        Some(IntentLineageReadOnlyReasonV1::MissingParentMutation)
    );
    Ok(())
}

fn start_replanned_task_execution(
    session: &mut Session,
) -> Result<(
    IntentVersionRef,
    IntentCriterionId,
    IntentExecutionId,
    ChangeSet,
)> {
    let admission = root_admission(session, "stack-task-replan")?;
    let intent_ref = admission.plan().intents[0].intent_ref.clone();
    let criterion_id = admission.plan().intents[0].acceptance_criteria[0]
        .criterion_id
        .clone();
    let task_id = TaskId::new("task-replan")?;
    let step_id = TaskStepId::new("implement-retry")?;
    let plan_v1 = bind_task_plan_intents(
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
    append_task_intent_plan_admission(session, &admission, plan_v1.clone())?;

    let mut plan_v2 = plan_v1;
    plan_v2.plan_version = 2;
    plan_v2.steps[0].title = "Implement retry after replan".to_owned();
    session.append_control(ControlEntry::TaskPlan(plan_v2))?;

    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(2),
        Some(&step_id),
        1,
    )?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Step,
            ordinal: 1,
            plan_version: Some(2),
            step_id: Some(step_id.clone()),
            role: AgentRole::Executor,
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    let execution_id = append_task_intent_execution_binding(
        session,
        intent_ref.clone(),
        &task_id,
        2,
        &step_id,
        &attempt_id,
    )?
    .execution_id
    .context("Task execution id is present")?;
    let change_set = sample_changeset("changeset-task-replan")?;
    session.append_control(ControlEntry::ChangeSetProposed(change_set.clone()))?;
    append_intent_changeset_binding(session, &execution_id, vec![change_set.id.clone()])?;
    Ok((intent_ref, criterion_id, execution_id, change_set))
}

fn append_task_promotion(
    store: &JsonlSessionStore,
    session: &mut Session,
    change_set: &ChangeSet,
    target: IntegrationPromotionTarget,
) -> Result<()> {
    let base_representation = IntegrationBaseRepresentation::CleanCommit {
        base_commit: "a".repeat(40),
    };
    let facts = IntegrationProposalFacts::from_changeset(
        change_set,
        base_representation,
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        "artifact:changeset",
        Vec::new(),
    )?;
    let proposal = IntegrationProposalSpec::from_changeset(
        change_set,
        TaskStepId::new("implement-retry")?,
        "snapshot-task-base".to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        "scope-task",
        facts,
    )?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("integration-task-replan")?,
        TaskId::new("task-replan")?,
        2,
        vec![proposal],
    )?;
    session.append_control(ControlEntry::IntegrationPlanRecorded(
        IntegrationPlanRecorded { plan: plan.clone() },
    ))?;
    let effect = match &target {
        IntegrationPromotionTarget::WorkspaceApply { .. } => {
            let recorder = MutationEventRecorder::new(store.clone());
            let file = &change_set.files[0];
            let operation_id = "operation-task-promotion".to_owned();
            let batch_id = "batch-task-promotion".to_owned();
            let subject = MutationSubject::File {
                path: file.path.clone().into(),
                file_type: FileType::File,
            };
            recorder.append_batch_started(
                &batch_id,
                "operation-task-promotion-batch",
                std::slice::from_ref(&subject),
            )?;
            recorder.append_prepared(&MutationPrepared {
                operation_id: operation_id.clone(),
                batch_id: Some(batch_id.clone()),
                tool_call_id: Some("tool-task-promotion".to_owned()),
                causation_event_id: "event-task-promotion".to_owned(),
                subject: subject.clone(),
                before_hash: file.before_hash.clone(),
                intended_after_hash: file.after_hash.clone(),
                snapshot_coverage: SnapshotCoverage::Unsupported,
                workspace_id: "workspace-demo".to_owned(),
                base_workspace_revision: 1,
                sync_class: MutationSyncClass::RecoveryCritical,
            })?;
            recorder.append_committed(&MutationCommitted {
                operation_id: operation_id.clone(),
                batch_id: Some(batch_id.clone()),
                workspace_id: Some("workspace-demo".to_owned()),
                observed_after_hash: file.after_hash.clone(),
                workspace_revision: 2,
                workspace_snapshot_id: "snapshot-single-file".to_owned(),
                committed_subject: subject,
            })?;
            recorder.append_batch_finished(
                &batch_id,
                MutationBatchStatus::Applied,
                std::slice::from_ref(&operation_id),
                &[],
            )?;
            session.append_control(ControlEntry::ChangeSetApplied(ChangeSetResult {
                id: change_set.id.clone(),
                status: ChangeSetResultStatus::Applied,
                file_results: vec![ChangeSetFileResult {
                    path: file.path.clone(),
                    action: file.action,
                    status: ChangeSetFileResultStatus::Applied,
                    message: None,
                    validations: Vec::new(),
                }],
                message: None,
            }))?;
            IntegrationPromotionEffect::WorkspaceApplied {
                promoted_snapshot_id: "snapshot-task-promoted".to_owned(),
                promoted_revision: 2,
            }
        }
        IntegrationPromotionTarget::GitRefAdvance {
            expected_old_oid,
            candidate_oid,
            ..
        } => IntegrationPromotionEffect::GitRefAdvanced {
            old_oid: expected_old_oid.clone(),
            new_oid: candidate_oid.clone(),
        },
    };
    session.append_control(ControlEntry::IntegrationPromotionRecorded(
        IntegrationPromotionRecorded {
            plan_id: plan.plan_id,
            attempt_id: None,
            status: IntegrationPromotionStatus::Promoted,
            preview_digest: format!("sha256:{}", "d".repeat(64)),
            target,
            authority_nonce: None,
            effect: Some(effect),
            recovery_binding: None,
            reason: None,
            recorded_at_unix_ms: 0,
        },
    ))?;
    Ok(())
}

#[test]
fn task_replan_preserves_intent_ref_and_workspace_apply_proves_parent_lineage() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session) = session_at(&temp.path().join("session.jsonl"))?;
    let (intent_ref, _criterion_id, execution_id, change_set) =
        start_replanned_task_execution(&mut session)?;
    append_task_promotion(
        &store,
        &mut session,
        &change_set,
        IntegrationPromotionTarget::WorkspaceApply {
            expected_snapshot_id: "snapshot-task-base".to_owned(),
            expected_revision: 1,
        },
    )?;

    let projection = session.intent_lineage_projection()?;
    let execution = projection
        .execution(&execution_id)
        .context("execution exists")?;
    assert!(matches!(
        execution.binding.origin,
        IntentExecutionOriginV1::Task {
            task_plan_version: 2,
            ..
        }
    ));
    assert_eq!(execution.binding.intent_ref, intent_ref);
    assert_eq!(
        execution.parent_snapshot_id.as_deref(),
        Some("snapshot-task-promoted")
    );
    assert_eq!(
        projection.summary_for(&intent_ref).application_state,
        Some(IntentApplicationState::NeedsReview)
    );
    Ok(())
}

#[test]
fn git_ref_only_task_promotion_remains_read_only_provenance() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session) = session_at(&temp.path().join("session.jsonl"))?;
    let (intent_ref, _criterion_id, _execution_id, change_set) =
        start_replanned_task_execution(&mut session)?;
    append_task_promotion(
        &store,
        &mut session,
        &change_set,
        IntegrationPromotionTarget::GitRefAdvance {
            target_ref: "refs/heads/main".to_owned(),
            expected_old_oid: "a".repeat(40),
            candidate_oid: "b".repeat(40),
        },
    )?;
    let summary = session
        .intent_lineage_projection()?
        .summary_for(&intent_ref);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::ReadOnly)
    );
    assert_eq!(
        summary.read_only_reason,
        Some(IntentLineageReadOnlyReasonV1::GitRefAdvance)
    );
    Ok(())
}

fn append_verification_receipt(
    session: &mut Session,
    intent_ref: &IntentVersionRef,
    criterion_id: &IntentCriterionId,
    change_set_id: &ChangeSetId,
    receipt_id: &str,
    scoped: bool,
) -> Result<(IntentDigest, String)> {
    let mut check = CheckSpec::new(
        format!("check-{receipt_id}"),
        CheckCommand::shell("cargo test -p retry"),
        ToolEffect::ReadOnly,
        "scope-chat",
    );
    if scoped {
        check = check.with_intent_scope(intent_ref.clone(), vec![criterion_id.clone()])?;
    }
    let policy = VerificationPolicy {
        required_checks: vec![check.clone()],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked("scope-chat"),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: Some(60_000),
        auto_run: VerificationAutoRunPolicy::Manual,
    };
    let policy_entry = VerificationPolicyChangedEntry::new(
        EvidenceScope::Run("root-run-1".to_owned()),
        policy,
        format!("event-policy-source-{receipt_id}"),
    )?;
    let policy_digest = IntentDigest::new(policy_entry.policy_hash.clone())?;
    session.append_control(ControlEntry::VerificationPolicyChanged(policy_entry))?;
    let receipt = VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id: receipt_id.to_owned(),
            source_session_id: session.session_scope_id().to_owned(),
            source_event_id: format!("event-check-finished-{receipt_id}"),
            source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
            scope: EvidenceScope::Run("root-run-1".to_owned()),
            producer_tool_call: Some("tool-call-check".to_owned()),
            workspace_revision: Some(1),
            workspace_snapshot_id: Some("snapshot-chat-1".to_owned()),
            policy_hash: Some(policy_digest.as_str().to_owned()),
            changeset_id: Some(change_set_id.as_str().to_owned()),
            status: ReceiptStatus::Succeeded,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence: 1,
        },
        binding: VerificationBinding {
            workspace_id: "workspace-demo".to_owned(),
            workspace_snapshot_id: "snapshot-chat-1".to_owned(),
            verification_scope_hash: "scope-chat".to_owned(),
            check_spec_hash: check.check_spec_hash,
            environment_fingerprint: "env-test".to_owned(),
            sandbox_profile_hash: "sandbox-test".to_owned(),
            execution_backend: Some(ExecutionBackendKind::Local),
            execution_backend_capabilities: None,
            execution_network: Default::default(),
            workspace_trust_snapshot_id: "trust-test".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        check_spec_id: check.check_spec_id,
        check_status: ReceiptStatus::Succeeded,
        failure_reason: None,
        mutates_verification_scope: false,
    };
    session.append_control(ControlEntry::VerificationRecorded(
        VerificationRecordedEntry { receipt },
    ))?;
    let records = JsonlSessionStore::read_event_records(
        session
            .store_path()
            .context("test session has durable path")?,
    )?;
    let facts = DurableLineageFacts::from_records(&records)?;
    let source_event_id = facts
        .verification_receipts
        .iter()
        .find(|fact| fact.value.receipt.receipt.receipt_id == receipt_id)
        .context("verification receipt event exists")?
        .event_id
        .clone();
    Ok((policy_digest, source_event_id))
}

fn criterion_evidence(
    intent_ref: &IntentVersionRef,
    criterion_id: &IntentCriterionId,
    change_set_id: &ChangeSetId,
    receipt_id: &str,
    policy_digest: IntentDigest,
    source_event_id: String,
    level: IntentCriterionEvidenceLevel,
) -> IntentCriterionEvidenceV1 {
    IntentCriterionEvidenceV1 {
        intent_ref: intent_ref.clone(),
        criterion_id: criterion_id.clone(),
        level,
        receipt_id: receipt_id.to_owned(),
        parent_snapshot_id: "snapshot-chat-1".to_owned(),
        verification_policy_digest: policy_digest,
        changeset_ids: vec![change_set_id.as_str().to_owned()],
        source_event_id,
    }
}

#[test]
fn model_association_is_advisory_until_check_declares_exact_criterion_scope() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session) = session_at(&temp.path().join("session.jsonl"))?;
    let (intent_ref, criterion_id, execution_id) =
        start_chat_execution(&mut session, "stack-evidence-level")?;
    let change_set_id = append_chat_mutation(
        &store,
        &mut session,
        &execution_id,
        "operation-chat-evidence",
        "snapshot-chat-1",
    )?;
    let (policy_digest, source_event_id) = append_verification_receipt(
        &mut session,
        &intent_ref,
        &criterion_id,
        &change_set_id,
        "receipt-advisory",
        false,
    )?;
    let advisory = criterion_evidence(
        &intent_ref,
        &criterion_id,
        &change_set_id,
        "receipt-advisory",
        policy_digest.clone(),
        source_event_id.clone(),
        IntentCriterionEvidenceLevel::Advisory,
    );
    append_intent_verification_evidence(&session, vec![advisory])?;
    let forged_system = criterion_evidence(
        &intent_ref,
        &criterion_id,
        &change_set_id,
        "receipt-advisory",
        policy_digest,
        source_event_id,
        IntentCriterionEvidenceLevel::SystemVerified,
    );
    assert!(
        append_intent_verification_evidence(&session, vec![forged_system])
            .expect_err("unscoped model association cannot become system verified")
            .to_string()
            .contains("explicitly cover")
    );

    let (policy_digest, source_event_id) = append_verification_receipt(
        &mut session,
        &intent_ref,
        &criterion_id,
        &change_set_id,
        "receipt-system",
        true,
    )?;
    let system = criterion_evidence(
        &intent_ref,
        &criterion_id,
        &change_set_id,
        "receipt-system",
        policy_digest,
        source_event_id,
        IntentCriterionEvidenceLevel::SystemVerified,
    );
    append_intent_verification_evidence(&session, vec![system])?;
    let summary = session
        .intent_lineage_projection()?
        .summary_for(&intent_ref);
    assert_eq!(summary.advisory_criterion_count, 1);
    assert_eq!(summary.system_verified_criterion_count, 1);
    Ok(())
}

#[test]
fn later_parent_mutation_makes_system_receipt_stale_and_projection_read_only() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session) = session_at(&temp.path().join("session.jsonl"))?;
    let (intent_ref, criterion_id, execution_id) =
        start_chat_execution(&mut session, "stack-stale-evidence")?;
    let change_set_id = append_chat_mutation(
        &store,
        &mut session,
        &execution_id,
        "operation-chat-stale",
        "snapshot-chat-1",
    )?;
    let (policy_digest, source_event_id) = append_verification_receipt(
        &mut session,
        &intent_ref,
        &criterion_id,
        &change_set_id,
        "receipt-before-drift",
        true,
    )?;
    append_intent_verification_evidence(
        &session,
        vec![criterion_evidence(
            &intent_ref,
            &criterion_id,
            &change_set_id,
            "receipt-before-drift",
            policy_digest,
            source_event_id,
            IntentCriterionEvidenceLevel::SystemVerified,
        )],
    )?;
    MutationEventRecorder::new(store).append_committed(&MutationCommitted {
        operation_id: "operation-human-drift".to_owned(),
        batch_id: None,
        workspace_id: Some("workspace-demo".to_owned()),
        observed_after_hash: Some(format!("sha256:{}", "e".repeat(64))),
        workspace_revision: 2,
        workspace_snapshot_id: "snapshot-chat-2".to_owned(),
        committed_subject: MutationSubject::File {
            path: "src/other.rs".into(),
            file_type: FileType::File,
        },
    })?;

    let projection = session.intent_lineage_projection()?;
    let summary = projection.summary_for(&intent_ref);
    assert_eq!(summary.system_verified_criterion_count, 0);
    assert_eq!(
        summary.application_state,
        Some(IntentApplicationState::ReadOnly)
    );
    assert_eq!(
        summary.read_only_reason,
        Some(IntentLineageReadOnlyReasonV1::StaleParentSnapshot)
    );
    assert!(matches!(
        session.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { stack, .. }
            if stack.authority_state == IntentAuthorityState::ReadOnlyProvenance
                && stack.intents[0].system_verified_criterion_count == 0
    ));
    Ok(())
}
