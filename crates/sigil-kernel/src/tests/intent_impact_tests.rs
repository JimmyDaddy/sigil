use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::tempdir;

use super::*;
use crate::*;

fn session_at(path: &Path) -> Result<(JsonlSessionStore, Session)> {
    let store = JsonlSessionStore::new(path)?;
    let session = Session::load_from_store("test-provider", "test-model", store.clone())?;
    Ok((store, session))
}

fn proposal_three() -> Result<IntentPlanProposalV1> {
    let mut proposal = IntentPlanProposalV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        proposal_id: "proposal-impact-three".to_owned(),
        source_turn_id: "turn-impact-initial".to_owned(),
        intents: vec![
            proposal_unit("base", "Base", Vec::new()),
            proposal_unit("middle", "Middle", vec!["base"]),
            proposal_unit("leaf", "Leaf", vec!["middle"]),
        ],
        proposal_digest: zero_test_digest()?,
    };
    proposal.proposal_digest = proposal.computed_digest()?;
    proposal.validate_contract()?;
    Ok(proposal)
}

fn proposal_unit(alias: &str, title: &str, depends_on_aliases: Vec<&str>) -> IntentProposalUnitV1 {
    IntentProposalUnitV1 {
        intent_alias: alias.to_owned(),
        title: title.to_owned(),
        statement: format!("Implement {title}."),
        acceptance_criteria: vec![IntentProposalCriterionV1 {
            criterion_alias: format!("{alias}-criterion"),
            statement: format!("{title} is verified."),
            required: true,
        }],
        depends_on_aliases: depends_on_aliases.into_iter().map(str::to_owned).collect(),
    }
}

fn setup_task_stack(
    root: &Path,
) -> Result<(
    JsonlSessionStore,
    Session,
    std::path::PathBuf,
    Vec<IntentVersionRef>,
)> {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let (store, mut session) = session_at(&root.join("session.jsonl"))?;
    let proposal = proposal_three()?;
    let authority = IntentAcceptanceAuthorityV1::explicit_user_confirmation(
        &proposal.source_turn_id,
        "decision-impact-initial",
        proposal.proposal_digest.clone(),
    )?;
    let admission = admit_suggested_decomposition(
        &IntentAdmissionContextV1::initial(
            IntentStackId::new("stack-impact")?,
            stable_workspace_id(&workspace)?,
            session.session_scope_id(),
        )?,
        &proposal,
        &authority,
    )?;
    let refs = admission
        .plan()
        .intents
        .iter()
        .map(|intent| intent.intent_ref.clone())
        .collect::<Vec<_>>();
    append_task_intent_plan_admission(&mut session, &admission, task_plan_for_refs(&refs, 1)?)?;
    Ok((store, session, workspace, refs))
}

fn task_plan_for_refs(refs: &[IntentVersionRef], version: u32) -> Result<TaskPlanEntry> {
    let steps = refs
        .iter()
        .enumerate()
        .map(|(index, intent_ref)| {
            Ok(TaskStepSpec {
                step_id: TaskStepId::new(format!("impact-step-{index}"))?,
                title: format!("Impact step {index}"),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: if index == 0 {
                    Vec::new()
                } else {
                    vec![TaskStepId::new(format!("impact-step-{}", index - 1))?]
                },
                intent_refs: vec![intent_ref.clone()],
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TaskPlanEntry {
        task_id: TaskId::new("task-impact")?,
        plan_version: version,
        status: TaskPlanStatus::Accepted,
        steps,
        reason: Some("Intent impact replan".to_owned()),
    })
}

fn revision(
    target: IntentVersionRef,
    depends_on: Vec<IntentId>,
    source_turn_id: &str,
) -> Result<IntentRevisionProposalV1> {
    IntentRevisionProposalV1::new(
        format!("revision-{}", target.intent_id.as_str()),
        source_turn_id,
        target,
        "Revised base",
        "Implement the revised base behavior.",
        vec![IntentProposalCriterionV1 {
            criterion_alias: "revised-criterion".to_owned(),
            statement: "The revised base behavior is verified.".to_owned(),
            required: true,
        }],
        depends_on,
    )
}

#[test]
fn impact_preview_computes_leaf_and_transitive_dependency_closure() -> Result<()> {
    let temp = tempdir()?;
    let (_store, session, workspace, refs) = setup_task_stack(temp.path())?;

    let base_revision = revision(refs[0].clone(), Vec::new(), "turn-revise-base")?;
    let revise = preview_intent_revision(&session, &workspace, &base_revision)?;
    assert!(!revise.operation.target_is_leaf);
    assert_eq!(revise.operation.target_intents, refs);
    assert!(revise.operation.file_effects.is_empty());
    assert_eq!(
        revise
            .application_impacts
            .iter()
            .map(|impact| impact.application_state)
            .collect::<Vec<_>>(),
        vec![
            IntentApplicationState::NeedsRebuild,
            IntentApplicationState::NeedsReview,
            IntentApplicationState::NeedsReview,
        ]
    );

    let replace = preview_intent_replace(&session, &workspace, &refs[0])?;
    assert_eq!(replace.operation.target_intents, refs);
    assert!(
        replace
            .application_impacts
            .iter()
            .all(|impact| impact.application_state == IntentApplicationState::NeedsRebuild)
    );

    let leaf_revision = revision(
        refs[2].clone(),
        vec![refs[1].intent_id.clone()],
        "turn-revise-leaf",
    )?;
    let leaf = preview_intent_revision(&session, &workspace, &leaf_revision)?;
    assert!(leaf.operation.target_is_leaf);
    assert_eq!(leaf.operation.target_intents, vec![refs[2].clone()]);
    Ok(())
}

#[test]
fn accepted_revision_supersedes_exact_version_replans_and_survives_restart() -> Result<()> {
    let temp = tempdir()?;
    let (store, mut session, workspace, refs) = setup_task_stack(temp.path())?;
    let proposal = revision(refs[0].clone(), Vec::new(), "turn-revise-accepted")?;
    let preview = preview_intent_revision(&session, &workspace, &proposal)?;
    let authority = IntentRevisionAuthorityV1::explicit_user_confirmation(
        &proposal,
        &preview,
        "decision-revise-accepted",
    )?;
    let mut successor_refs = refs.clone();
    successor_refs[0] = proposal.replacement_ref()?;
    let successor_task_plan = task_plan_for_refs(&successor_refs, 2)?;
    let outcome = accept_intent_revision(
        &mut session,
        &workspace,
        &proposal,
        &preview,
        &authority,
        Some(successor_task_plan.clone()),
    )?;
    assert!(outcome.appended);
    assert!(
        !accept_intent_revision(
            &mut session,
            &workspace,
            &proposal,
            &preview,
            &authority,
            Some(successor_task_plan.clone()),
        )?
        .appended,
        "exact revision retry must be idempotent"
    );
    let mut conflicting_retry = successor_task_plan;
    conflicting_retry.reason = Some("conflicting retry".to_owned());
    assert!(
        accept_intent_revision(
            &mut session,
            &workspace,
            &proposal,
            &preview,
            &authority,
            Some(conflicting_retry),
        )
        .expect_err("same binding cannot hide a conflicting TaskPlan")
        .to_string()
        .contains("conflicts")
    );

    let projection = session.intent_stack_projection()?;
    let accepted = projection
        .latest_accepted_plan()
        .context("successor plan is active")?;
    assert_eq!(accepted.plan.stack_version.get(), 2);
    assert_eq!(accepted.plan.intents[0].intent_ref, successor_refs[0]);
    assert_eq!(accepted.plan.intents[0].supersedes.as_ref(), Some(&refs[0]));
    assert!(
        projection
            .accepted_plan(IntentStackVersion::new(1)?)
            .is_some(),
        "immutable predecessor remains replayable"
    );
    let PublicIntentStackStateV1::Available { stack, .. } = session.public_intent_stack_state()?
    else {
        panic!("accepted successor must render");
    };
    assert_eq!(
        stack.intents[0].application_state,
        IntentApplicationState::NeedsRebuild
    );
    assert_eq!(
        stack.intents[1].application_state,
        IntentApplicationState::NeedsReview
    );
    assert_eq!(
        stack.intents[2].application_state,
        IntentApplicationState::NeedsReview
    );

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let tail = records
        .iter()
        .rev()
        .take(4)
        .map(|record| record.stored_event().event_kind())
        .collect::<Vec<_>>();
    assert_eq!(
        tail,
        vec![
            Some(DurableEventType::TaskStatusChanged),
            Some(DurableEventType::IntentVersionSuperseded),
            Some(DurableEventType::IntentPlanAccepted),
            Some(DurableEventType::IntentPlanRecorded),
        ]
    );

    drop(session);
    drop(store);
    let (_reopened_store, reopened) = session_at(&temp.path().join("session.jsonl"))?;
    assert_eq!(
        reopened
            .intent_stack_projection()?
            .latest_accepted_plan()
            .context("restarted successor remains active")?
            .plan
            .stack_version
            .get(),
        2
    );
    assert!(matches!(
        reopened.public_intent_stack_state()?,
        PublicIntentStackStateV1::Available { .. }
    ));
    Ok(())
}

#[test]
fn revision_cycle_is_rejected_before_append_and_crash_prefix_stays_incomplete() -> Result<()> {
    let cycle_temp = tempdir()?;
    let (cycle_store, mut cycle_session, workspace, refs) = setup_task_stack(cycle_temp.path())?;
    let cycle = revision(
        refs[0].clone(),
        vec![refs[2].intent_id.clone()],
        "turn-revise-cycle",
    )?;
    let cycle_preview = preview_intent_revision(&cycle_session, &workspace, &cycle)?;
    let cycle_authority = IntentRevisionAuthorityV1::explicit_user_confirmation(
        &cycle,
        &cycle_preview,
        "decision-revise-cycle",
    )?;
    let before = JsonlSessionStore::read_event_records(cycle_store.path())?.len();
    assert!(
        accept_intent_revision(
            &mut cycle_session,
            &workspace,
            &cycle,
            &cycle_preview,
            &cycle_authority,
            Some(task_plan_for_refs(&refs, 2)?),
        )
        .expect_err("cycle must fail before durable append")
        .to_string()
        .contains("cycle")
    );
    assert_eq!(
        JsonlSessionStore::read_event_records(cycle_store.path())?.len(),
        before
    );

    let crash_temp = tempdir()?;
    let (crash_store, mut crash_session, crash_workspace, crash_refs) =
        setup_task_stack(crash_temp.path())?;
    let crash = revision(crash_refs[0].clone(), Vec::new(), "turn-revise-crash")?;
    let crash_preview = preview_intent_revision(&crash_session, &crash_workspace, &crash)?;
    let crash_authority = IntentRevisionAuthorityV1::explicit_user_confirmation(
        &crash,
        &crash_preview,
        "decision-revise-crash",
    )?;
    let mut successor_refs = crash_refs.clone();
    successor_refs[0] = crash.replacement_ref()?;
    accept_intent_revision(
        &mut crash_session,
        &crash_workspace,
        &crash,
        &crash_preview,
        &crash_authority,
        Some(task_plan_for_refs(&successor_refs, 2)?),
    )?;
    let content = fs::read_to_string(crash_store.path())?;
    let mut lines = content.lines().collect::<Vec<_>>();
    let removed = lines.pop().context("successor has a TaskPlan tail")?;
    assert_eq!(
        StoredEvent::from_json_str(removed)?.event_kind(),
        Some(DurableEventType::TaskStatusChanged)
    );
    fs::write(crash_store.path(), format!("{}\n", lines.join("\n")))?;
    let projection = IntentStackProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(crash_store.path())?,
    )?;
    assert!(projection.has_incomplete_task_acceptance());
    assert_eq!(
        projection
            .latest_accepted_plan()
            .context("predecessor remains active")?
            .plan
            .stack_version
            .get(),
        1
    );
    assert!(projection.public_state().is_err());
    Ok(())
}

#[test]
fn later_unrelated_revision_does_not_reinvalidate_reexecuted_dependency_branch() -> Result<()> {
    let temp = tempdir()?;
    let (_store, mut session, workspace, refs) = setup_task_stack(temp.path())?;
    let base_revision = revision(refs[0].clone(), Vec::new(), "turn-revise-base-v2")?;
    let base_preview = preview_intent_revision(&session, &workspace, &base_revision)?;
    let base_authority = IntentRevisionAuthorityV1::explicit_user_confirmation(
        &base_revision,
        &base_preview,
        "decision-revise-base-v2",
    )?;
    let mut refs_v2 = refs.clone();
    refs_v2[0] = base_revision.replacement_ref()?;
    accept_intent_revision(
        &mut session,
        &workspace,
        &base_revision,
        &base_preview,
        &base_authority,
        Some(task_plan_for_refs(&refs_v2, 2)?),
    )?;
    let projection_v2 = session.intent_stack_projection()?;
    let accepted_v2 = projection_v2
        .latest_accepted_plan()
        .context("version two is active")?;

    let leaf_revision = revision(
        refs_v2[2].clone(),
        vec![refs_v2[1].intent_id.clone()],
        "turn-revise-leaf-v3",
    )?;
    let leaf_preview = preview_intent_revision(&session, &workspace, &leaf_revision)?;
    let leaf_authority = IntentRevisionAuthorityV1::explicit_user_confirmation(
        &leaf_revision,
        &leaf_preview,
        "decision-revise-leaf-v3",
    )?;
    let mut refs_v3 = refs_v2.clone();
    refs_v3[2] = leaf_revision.replacement_ref()?;
    accept_intent_revision(
        &mut session,
        &workspace,
        &leaf_revision,
        &leaf_preview,
        &leaf_authority,
        Some(task_plan_for_refs(&refs_v3, 3)?),
    )?;

    let projection_v3 = session.intent_stack_projection()?;
    let mut lineage = IntentLineageProjectionV1::default();
    let execution_id = IntentExecutionId::new("execution-middle-after-v2")?;
    lineage.execution_order.push(execution_id.clone());
    lineage.executions.insert(
        execution_id.clone(),
        IntentExecutionLineageV1 {
            stack_version: IntentStackVersion::new(2)?,
            binding: IntentExecutionBindingV1 {
                execution_id,
                intent_ref: refs_v2[1].clone(),
                origin: IntentExecutionOriginV1::Task {
                    task_id: "task-impact".to_owned(),
                    task_plan_version: 2,
                    step_id: "impact-step-1".to_owned(),
                    attempt_id: Some("attempt-middle-v2".to_owned()),
                },
                binding_kind: IntentExecutionBindingKind::Mutation,
                source_event_id: "source-middle-v2".to_owned(),
            },
            binding_event_id: "binding-middle-v2".to_owned(),
            binding_stream_sequence: accepted_v2.accepted_stream_sequence.saturating_add(1),
            changeset_ids: Vec::new(),
            parent_mutation_event_id: None,
            parent_snapshot_id: None,
            read_only_reason: Some(IntentLineageReadOnlyReasonV1::MissingChangeSet),
        },
    );
    let PublicIntentStackStateV1::Available { stack, .. } =
        projection_v3.public_state_with_lineage(&lineage)?
    else {
        panic!("version three must render");
    };
    assert_eq!(stack.stack_version.get(), 3);
    assert_eq!(
        stack.intents[1].application_state,
        IntentApplicationState::ReadOnly,
        "revision of another branch must not reset an already satisfied dependency frontier"
    );
    Ok(())
}

#[test]
fn workspace_scope_and_explicit_fork_adoption_are_fail_closed() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("tracked.txt"), "same\n")?;
    run_git(&workspace, &["init"])?;
    run_git(&workspace, &["add", "tracked.txt"])?;
    run_git(
        &workspace,
        &[
            "-c",
            "user.name=Sigil Test",
            "-c",
            "user.email=sigil@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    )?;
    let destination_workspace = temp.path().join("workspace-clone");
    let clone_output = Command::new("git")
        .arg("clone")
        .arg(&workspace)
        .arg(&destination_workspace)
        .output()?;
    if !clone_output.status.success() {
        bail!(
            "git clone failed: {}",
            String::from_utf8_lossy(&clone_output.stderr)
        );
    }
    let destination_tracked_content = fs::read(destination_workspace.join("tracked.txt"))?;

    let (_source_store, source) = session_at(&temp.path().join("source.jsonl"))?;
    let root_admission = admit_user_declared_root(
        &IntentAdmissionContextV1::initial(
            IntentStackId::new("stack-adoption-source")?,
            stable_workspace_id(&workspace)?,
            source.session_scope_id(),
        )?,
        UserDeclaredIntentV1 {
            title: "Adopt provenance".to_owned(),
            statement: "Preserve exact fork provenance.".to_owned(),
            acceptance_criteria: vec![IntentProposalCriterionV1 {
                criterion_alias: "adoption-check".to_owned(),
                statement: "Fork provenance remains read only until re-execution.".to_owned(),
                required: true,
            }],
        },
        &IntentAcceptanceAuthorityV1::user_declared_root(
            "turn-adoption-source",
            "event-adoption-source",
        )?,
    )?;
    append_chat_root_intent_admission(&source, &root_admission)?;

    let (_destination_store, mut destination) = session_at(&temp.path().join("destination.jsonl"))?;
    destination.append_durable_event(
        DurableEventType::ConversationForked,
        EventClass::Critical,
        serde_json::to_value(ConversationForked {
            fork_id: "fork-intent-adoption".to_owned(),
            parent_session_ref: SessionRef::new_relative("source.jsonl")?,
            source_session_id: source.session_scope_id().to_owned(),
            source_turn_index: 0,
            source_boundary_event_id: "source-boundary".to_owned(),
            source_boundary_stream_sequence: 1,
            source_turn_digest: "source-turn-digest".to_owned(),
            source_checkpoint_id: None,
            source_checkpoint_digest: None,
            destination_session_id: destination.session_scope_id().to_owned(),
            copied_message_count: 0,
            copied_external_provenance_count: 0,
        })?,
    )?;

    assert!(
        IntentAdoptionAuthorityV1::capture(
            &source,
            &destination,
            &workspace,
            &destination_workspace,
            "branch-a",
            "branch-b",
            "turn-adopt",
            "decision-adopt-mismatch",
            IntentStackId::new("stack-adoption-target-mismatch")?,
        )
        .expect_err("different branch lineage must not mint adoption authority")
        .to_string()
        .contains("branch lineage")
    );
    let authority = IntentAdoptionAuthorityV1::capture(
        &source,
        &destination,
        &workspace,
        &destination_workspace,
        "branch-a",
        "branch-a",
        "turn-adopt",
        "decision-adopt",
        IntentStackId::new("stack-adoption-target")?,
    )?;
    assert!(
        adopt_forked_intent_stack(
            &source,
            &destination,
            &workspace,
            &destination_workspace,
            "branch-b",
            "branch-b",
            &authority,
        )
        .expect_err("branch switch must invalidate captured adoption")
        .to_string()
        .contains("proof changed")
    );
    fs::write(destination_workspace.join("tracked.txt"), "drifted\n")?;
    assert!(
        adopt_forked_intent_stack(
            &source,
            &destination,
            &workspace,
            &destination_workspace,
            "branch-a",
            "branch-a",
            &authority,
        )
        .expect_err("snapshot drift must invalidate captured adoption")
        .to_string()
        .contains("proof changed")
    );
    fs::write(
        destination_workspace.join("tracked.txt"),
        destination_tracked_content,
    )?;
    assert!(
        adopt_forked_intent_stack(
            &source,
            &destination,
            &workspace,
            &destination_workspace,
            "branch-a",
            "branch-a",
            &authority,
        )?
        .appended
    );
    assert!(
        !adopt_forked_intent_stack(
            &source,
            &destination,
            &workspace,
            &destination_workspace,
            "branch-a",
            "branch-a",
            &authority,
        )?
        .appended,
        "exact adoption retry must be idempotent"
    );

    let PublicIntentStackStateV1::Available { stack, .. } =
        destination.public_intent_stack_state()?
    else {
        panic!("adopted stack must render");
    };
    assert_eq!(
        stack.authority_state,
        IntentAuthorityState::ReadOnlyProvenance
    );
    assert_eq!(
        stack.intents[0].application_state,
        IntentApplicationState::ReadOnly
    );
    let adopted_projection = destination.intent_stack_projection()?;
    let PublicIntentStackStateV1::Available {
        stack: out_of_scope,
        ..
    } = adopted_projection.public_state_for_workspace("workspace:other")?
    else {
        panic!("out-of-scope stack must remain inspectable");
    };
    assert_eq!(
        out_of_scope.authority_state,
        IntentAuthorityState::OutOfScope
    );
    assert_eq!(
        out_of_scope.intents[0].application_state,
        IntentApplicationState::OutOfScope
    );
    assert!(out_of_scope.intents[0].available_actions.is_empty());
    Ok(())
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        bail!(
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn zero_test_digest() -> Result<IntentDigest> {
    IntentDigest::new(format!(
        "{}{}",
        INTENT_CANONICAL_DIGEST_PREFIX,
        "0".repeat(64)
    ))
}
