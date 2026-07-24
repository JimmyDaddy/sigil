use anyhow::Result;

use crate::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk, ControlEntry,
    IntegrationBaseRepresentation, IntegrationConflictReason, IntegrationContentClass,
    IntegrationEffect, IntegrationFactGap, IntegrationLaneCandidate, IntegrationLaneChanged,
    IntegrationLaneStatus, IntegrationObservedEffect, IntegrationPlanId, IntegrationPlanRecorded,
    IntegrationProjection, IntegrationPromotionEffect, IntegrationPromotionRecorded,
    IntegrationPromotionStatus, IntegrationPromotionTarget, IntegrationProposalFacts,
    IntegrationProposalSpec, SessionLogEntry, TaskId, TaskStepId, build_integration_plan,
};

fn change_set(id: &str, paths: &[&str]) -> Result<ChangeSet> {
    Ok(ChangeSet {
        id: ChangeSetId::new(id)?,
        title: id.to_owned(),
        summary: format!("changes for {id}"),
        risk: ChangeSetRisk::Medium,
        files: paths
            .iter()
            .map(|path| ChangeSetFile {
                path: (*path).to_owned(),
                previous_path: None,
                action: ChangeSetFileAction::Update,
                risk: ChangeSetRisk::Medium,
                before_hash: Some(format!("before-{id}-{path}")),
                after_hash: Some(format!("after-{id}-{path}")),
                diff_hash: None,
                additions: 1,
                deletions: 1,
                validations: Vec::new(),
            })
            .collect(),
        validations: Vec::new(),
    })
}

fn proposal(
    id: &str,
    paths: &[&str],
    depends_on: &[&str],
    generated: &[&str],
    effect: IntegrationEffect,
) -> Result<IntegrationProposalSpec> {
    let change_set = change_set(id, paths)?;
    let facts = IntegrationProposalFacts::from_changeset(
        &change_set,
        IntegrationBaseRepresentation::CleanCommit {
            base_commit: "b".repeat(40),
        },
        IntegrationContentClass::Text,
        effect,
        Vec::new(),
        format!("artifact-{id}"),
        Vec::new(),
    )?;
    IntegrationProposalSpec::from_changeset(
        &change_set,
        TaskStepId::new(format!("step_{id}"))?,
        "snapshot-base".to_owned(),
        depends_on
            .iter()
            .map(|dependency| ChangeSetId::new(*dependency))
            .collect::<Result<Vec<_>>>()?,
        generated.iter().map(|path| (*path).to_owned()).collect(),
        effect,
        "scope-shared",
        facts,
    )
}

#[test]
fn conflict_graph_keeps_disjoint_proposals_in_parallel_lanes() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-1")?,
        TaskId::new("task_parallel")?,
        1,
        vec![
            proposal(
                "change-a",
                &["src/a.rs"],
                &[],
                &[],
                IntegrationEffect::Files,
            )?,
            proposal(
                "change-b",
                &["src/b.rs"],
                &[],
                &[],
                IntegrationEffect::Files,
            )?,
        ],
    )?;

    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.lanes.len(), 2);
    assert_eq!(plan.lanes[0].proposals[0].as_str(), "change-a");
    assert_eq!(plan.lanes[1].proposals[0].as_str(), "change-b");
    Ok(())
}

#[test]
fn conflict_graph_serializes_connected_path_dependency_and_global_domains() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-2")?,
        TaskId::new("task_conflict")?,
        3,
        vec![
            proposal(
                "change-a",
                &["src/shared.rs"],
                &[],
                &[],
                IntegrationEffect::Files,
            )?,
            proposal(
                "change-b",
                &["src/shared.rs"],
                &["change-a"],
                &[],
                IntegrationEffect::Files,
            )?,
            proposal(
                "change-c",
                &["src/c.rs"],
                &[],
                &["generated/schema.rs"],
                IntegrationEffect::GeneratedArtifacts,
            )?,
            proposal(
                "change-d",
                &["src/d.rs"],
                &[],
                &["generated/schema.rs"],
                IntegrationEffect::GeneratedArtifacts,
            )?,
            proposal(
                "change-e",
                &["Cargo.lock"],
                &[],
                &[],
                IntegrationEffect::Global,
            )?,
        ],
    )?;

    assert_eq!(plan.lanes.len(), 1);
    assert_eq!(plan.lanes[0].proposals.len(), 5);
    let reasons = plan
        .conflicts
        .iter()
        .flat_map(|edge| edge.reasons.iter().copied())
        .collect::<Vec<_>>();
    assert!(reasons.contains(&IntegrationConflictReason::ChangedPathOverlap));
    assert!(reasons.contains(&IntegrationConflictReason::TaskDependency));
    assert!(reasons.contains(&IntegrationConflictReason::GeneratedArtifactOverlap));
    assert!(reasons.contains(&IntegrationConflictReason::GlobalEffect));
    assert_eq!(plan.lanes[0].proposals[0].as_str(), "change-a");
    assert_eq!(plan.lanes[0].proposals[1].as_str(), "change-b");
    Ok(())
}

#[test]
fn conflict_graph_rejects_unsafe_paths_and_duplicate_ids() -> Result<()> {
    let unsafe_change = change_set("unsafe", &["../escape"])?;
    assert!(
        IntegrationProposalSpec::from_changeset(
            &unsafe_change,
            TaskStepId::new("step_unsafe")?,
            "snapshot-base".to_owned(),
            Vec::new(),
            Vec::new(),
            IntegrationEffect::Files,
            "scope",
            IntegrationProposalFacts::default(),
        )
        .is_err()
    );
    let duplicate = proposal(
        "duplicate",
        &["src/a.rs"],
        &[],
        &[],
        IntegrationEffect::Files,
    )?;
    assert!(
        build_integration_plan(
            IntegrationPlanId::new("plan-duplicate")?,
            TaskId::new("task_duplicate")?,
            1,
            vec![duplicate.clone(), duplicate],
        )
        .is_err()
    );
    let mut mismatched = proposal(
        "mismatched",
        &["src/mismatched.rs"],
        &[],
        &[],
        IntegrationEffect::Files,
    )?;
    mismatched.facts.declared_effect = IntegrationEffect::Global;
    assert!(
        build_integration_plan(
            IntegrationPlanId::new("plan-mismatched")?,
            TaskId::new("task_mismatched")?,
            1,
            vec![mismatched],
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn conflict_graph_is_stable_under_reverse_completion_order() -> Result<()> {
    let proposals = vec![
        proposal(
            "change-a",
            &["src/a.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?,
        proposal(
            "change-b",
            &["src/shared.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?,
        proposal(
            "change-c",
            &["src/shared.rs"],
            &["change-b"],
            &[],
            IntegrationEffect::Files,
        )?,
    ];
    let forward = build_integration_plan(
        IntegrationPlanId::new("plan-stable")?,
        TaskId::new("task_stable")?,
        1,
        proposals.clone(),
    )?;
    let reverse = build_integration_plan(
        IntegrationPlanId::new("plan-stable")?,
        TaskId::new("task_stable")?,
        1,
        proposals.into_iter().rev().collect(),
    )?;

    assert_eq!(forward, reverse);
    Ok(())
}

#[test]
fn incomplete_facts_force_one_manual_conflict_component() -> Result<()> {
    let mut incomplete = proposal(
        "change-incomplete",
        &["src/a.rs"],
        &[],
        &[],
        IntegrationEffect::Files,
    )?;
    incomplete.facts = IntegrationProposalFacts::default();
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-incomplete")?,
        TaskId::new("task_incomplete")?,
        1,
        vec![
            incomplete,
            proposal(
                "change-complete",
                &["src/b.rs"],
                &[],
                &[],
                IntegrationEffect::Files,
            )?,
        ],
    )?;

    assert!(plan.requires_manual_review());
    assert_eq!(plan.lanes.len(), 1);
    assert!(
        plan.conflicts[0]
            .reasons
            .contains(&IntegrationConflictReason::IncompleteEffectFacts)
    );
    Ok(())
}

#[test]
fn rename_source_and_verification_scope_are_conflict_facts() -> Result<()> {
    let mut renamed_change = change_set("change-rename", &["src/new.rs"])?;
    renamed_change.files[0].action = ChangeSetFileAction::Rename;
    renamed_change.files[0].previous_path = Some("src/old.rs".to_owned());
    let renamed_facts = IntegrationProposalFacts::from_changeset(
        &renamed_change,
        IntegrationBaseRepresentation::CleanCommit {
            base_commit: "b".repeat(40),
        },
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        "artifact-rename",
        Vec::new(),
    )?;
    let renamed = IntegrationProposalSpec::from_changeset(
        &renamed_change,
        TaskStepId::new("step_rename")?,
        "snapshot-base".to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        "scope-rename",
        renamed_facts,
    )?;
    let mut old_path = proposal(
        "change-old",
        &["src/old.rs"],
        &[],
        &[],
        IntegrationEffect::Files,
    )?;
    old_path.verification_scope_hash = "scope-old".to_owned();
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-rename")?,
        TaskId::new("task_rename")?,
        1,
        vec![renamed, old_path],
    )?;

    let reasons = &plan.conflicts[0].reasons;
    assert!(reasons.contains(&IntegrationConflictReason::ChangedPathOverlap));
    assert!(reasons.contains(&IntegrationConflictReason::VerificationScopeMismatch));
    Ok(())
}

#[test]
fn package_build_and_git_effects_are_explicit_edge_reasons() -> Result<()> {
    for (observed, expected) in [
        (
            IntegrationObservedEffect::Package,
            IntegrationConflictReason::PackageEffect,
        ),
        (
            IntegrationObservedEffect::Build,
            IntegrationConflictReason::BuildEffect,
        ),
        (
            IntegrationObservedEffect::Git,
            IntegrationConflictReason::GitEffect,
        ),
    ] {
        let mut affected = proposal(
            "change-affected",
            &["src/a.rs"],
            &[],
            &[],
            IntegrationEffect::Global,
        )?;
        affected.facts.observed_effects = vec![observed];
        let plan = build_integration_plan(
            IntegrationPlanId::new(format!("plan-{}", expected.as_str()))?,
            TaskId::new(format!("task_{}", expected.as_str()))?,
            1,
            vec![
                affected,
                proposal(
                    "change-other",
                    &["src/b.rs"],
                    &[],
                    &[],
                    IntegrationEffect::Files,
                )?,
            ],
        )?;
        assert!(plan.conflicts[0].reasons.contains(&expected));
    }
    Ok(())
}

#[test]
fn fact_builder_marks_missing_hashes_and_unknown_base_for_manual_review() -> Result<()> {
    let mut incomplete = change_set("change-gaps", &["src/lib.rs"])?;
    incomplete.files[0].before_hash = None;
    incomplete.files[0].after_hash = None;
    let facts = IntegrationProposalFacts::from_changeset(
        &incomplete,
        IntegrationBaseRepresentation::Unknown,
        IntegrationContentClass::Unknown,
        IntegrationEffect::Unknown,
        vec![IntegrationObservedEffect::Unknown],
        "",
        Vec::new(),
    )?;

    assert!(facts.requires_manual_review());
    assert!(
        facts
            .gaps
            .contains(&IntegrationFactGap::UnknownBaseRepresentation)
    );
    assert!(facts.gaps.contains(&IntegrationFactGap::MissingArtifactRef));
    assert!(facts.gaps.contains(&IntegrationFactGap::MissingBeforeHash));
    assert!(facts.gaps.contains(&IntegrationFactGap::MissingAfterHash));
    assert!(
        facts
            .gaps
            .contains(&IntegrationFactGap::UnknownContentClass)
    );
    assert!(
        facts
            .gaps
            .contains(&IntegrationFactGap::UnknownDeclaredEffect)
    );
    assert!(
        facts
            .gaps
            .contains(&IntegrationFactGap::UnknownObservedEffect)
    );
    Ok(())
}

#[test]
fn integration_projection_replays_lane_and_promotion_state() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-replay")?,
        TaskId::new("task_replay")?,
        1,
        vec![proposal(
            "change-replay",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let lane_id = plan.lanes[0].lane_id.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::IntegrationPlanRecorded(
            IntegrationPlanRecorded { plan: plan.clone() },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneChanged(
            IntegrationLaneChanged {
                plan_id: plan.plan_id.clone(),
                lane_id: lane_id.clone(),
                status: IntegrationLaneStatus::Ready,
                candidate: Some(IntegrationLaneCandidate::ManagedRef {
                    private_ref: "refs/sigil/integration/plan-replay/lane-1".to_owned(),
                    base_commit: "b".repeat(40),
                    candidate_commit: "a".repeat(40),
                    workspace_snapshot_id: "snapshot-lane".to_owned(),
                }),
                verification_check_ids: vec!["git-diff-check".to_owned()],
                reason: None,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationPromotionRecorded(
            IntegrationPromotionRecorded {
                plan_id: plan.plan_id.clone(),
                status: IntegrationPromotionStatus::Promoted,
                preview_digest: "preview-sha256".to_owned(),
                target: IntegrationPromotionTarget::WorkspaceApply {
                    expected_snapshot_id: "snapshot-base".to_owned(),
                    expected_revision: 3,
                },
                effect: Some(IntegrationPromotionEffect::WorkspaceApplied {
                    promoted_snapshot_id: "snapshot-parent-after".to_owned(),
                    promoted_revision: 4,
                }),
                reason: None,
            },
        )),
    ];

    let projection = IntegrationProjection::from_entries(&entries);
    let state = projection.latest().expect("integration state");
    assert!(!state.inconsistent);
    assert_eq!(
        state.lanes.get(&lane_id).map(|lane| lane.status),
        Some(IntegrationLaneStatus::Ready)
    );
    assert_eq!(
        state.promotions.last().map(|promotion| promotion.status),
        Some(IntegrationPromotionStatus::Promoted)
    );
    Ok(())
}

#[test]
fn integration_projection_rejects_mismatched_promotion_effect() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-mismatched-promotion")?,
        TaskId::new("task_mismatched_promotion")?,
        1,
        vec![proposal(
            "change-mismatched-promotion",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::IntegrationPlanRecorded(
            IntegrationPlanRecorded { plan: plan.clone() },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationPromotionRecorded(
            IntegrationPromotionRecorded {
                plan_id: plan.plan_id,
                status: IntegrationPromotionStatus::Promoted,
                preview_digest: "preview-sha256".to_owned(),
                target: IntegrationPromotionTarget::WorkspaceApply {
                    expected_snapshot_id: "snapshot-base".to_owned(),
                    expected_revision: 3,
                },
                effect: Some(IntegrationPromotionEffect::GitRefAdvanced {
                    old_oid: "b".repeat(40),
                    new_oid: "a".repeat(40),
                }),
                reason: None,
            },
        )),
    ];

    let projection = IntegrationProjection::from_entries(&entries);
    let state = projection.latest().expect("integration state");
    assert!(state.inconsistent);
    Ok(())
}
