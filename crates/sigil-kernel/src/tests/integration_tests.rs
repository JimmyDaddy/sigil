use anyhow::Result;

use crate::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk, ControlEntry,
    IntegrationConflictReason, IntegrationEffect, IntegrationLaneCandidate, IntegrationLaneChanged,
    IntegrationLaneStatus, IntegrationPlanId, IntegrationPlanRecorded, IntegrationProjection,
    IntegrationPromotionEffect, IntegrationPromotionRecorded, IntegrationPromotionStatus,
    IntegrationPromotionTarget, IntegrationProposalSpec, SessionLogEntry, TaskId, TaskStepId,
    build_integration_plan,
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
                before_hash: None,
                after_hash: None,
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
    IntegrationProposalSpec::from_changeset(
        &change_set(id, paths)?,
        TaskStepId::new(format!("step_{id}"))?,
        "snapshot-base".to_owned(),
        depends_on
            .iter()
            .map(|dependency| ChangeSetId::new(*dependency))
            .collect::<Result<Vec<_>>>()?,
        generated.iter().map(|path| (*path).to_owned()).collect(),
        effect,
        format!("scope-{id}"),
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
