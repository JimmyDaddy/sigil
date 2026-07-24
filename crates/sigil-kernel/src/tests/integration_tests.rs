use anyhow::Result;

use crate::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk, ControlEntry,
    EvidenceReceipt, EvidenceScope, ExecutionBackendCapabilities, ExecutionBackendKind,
    ExecutionNetworkReceipt, IntegrationBaseRepresentation, IntegrationConflictReason,
    IntegrationContentClass, IntegrationEffect, IntegrationFactGap, IntegrationLaneCandidate,
    IntegrationLaneChanged, IntegrationLaneCleanupRecorded, IntegrationLaneCleanupStatus,
    IntegrationLaneMemberApplied, IntegrationLaneMemberEffect, IntegrationLanePrepared,
    IntegrationLaneStatus, IntegrationLaneTarget, IntegrationLaneTerminal,
    IntegrationLaneVerificationLinked, IntegrationObservedEffect, IntegrationPlanId,
    IntegrationPlanRecorded, IntegrationProjection, IntegrationPromotionAttemptId,
    IntegrationPromotionEffect, IntegrationPromotionRecorded, IntegrationPromotionRecoveryBinding,
    IntegrationPromotionStatus, IntegrationPromotionTarget, IntegrationProposalFacts,
    IntegrationProposalSpec, ReceiptStatus, RedactionState, SessionLogEntry, TaskId,
    TaskParentVerificationRecorded, TaskPromotionAuthority, TaskPromotionAuthorityConsumed,
    TaskPromotionPreviewInput, TaskPromotionPreviewRecorded, TaskStepId, VerificationBinding,
    VerificationPolicy, VerificationReceipt, VerificationVerdict, build_integration_plan,
    build_task_promotion_preview,
};

fn integration_verification_receipt(
    check_spec_id: &str,
    verification_scope_hash: &str,
) -> VerificationReceipt {
    VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id: format!("receipt-{check_spec_id}"),
            source_session_id: "session-integration".to_owned(),
            source_event_id: format!("event-{check_spec_id}"),
            source_event_type: "check_finished".to_owned(),
            scope: EvidenceScope::Task("task-integration".to_owned()),
            producer_tool_call: None,
            workspace_revision: Some(0),
            workspace_snapshot_id: Some("snapshot-scope".to_owned()),
            policy_hash: Some("policy-integration".to_owned()),
            changeset_id: None,
            status: ReceiptStatus::Succeeded,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence: 1,
        },
        binding: VerificationBinding {
            workspace_id: "workspace-integration".to_owned(),
            workspace_snapshot_id: "snapshot-scope".to_owned(),
            verification_scope_hash: verification_scope_hash.to_owned(),
            check_spec_hash: format!("hash-{check_spec_id}"),
            environment_fingerprint: "environment-integration".to_owned(),
            sandbox_profile_hash: "sandbox-integration".to_owned(),
            execution_backend: Some(ExecutionBackendKind::Local),
            execution_backend_capabilities: Some(ExecutionBackendCapabilities::default()),
            execution_network: ExecutionNetworkReceipt::unknown("test local backend"),
            workspace_trust_snapshot_id: "trust-integration".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        check_spec_id: check_spec_id.to_owned(),
        check_status: ReceiptStatus::Succeeded,
        failure_reason: None,
        mutates_verification_scope: false,
    }
}

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

fn snapshot_proposal(id: &str, path: &str) -> Result<IntegrationProposalSpec> {
    let change_set = change_set(id, &[path])?;
    let base_representation = IntegrationBaseRepresentation::SnapshotWorkspace {
        base_commit: "b".repeat(40),
        overlay_digest: format!("sha256:{}", "d".repeat(64)),
    };
    let facts = IntegrationProposalFacts::from_changeset(
        &change_set,
        base_representation,
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        format!("artifact-{id}"),
        Vec::new(),
    )?;
    IntegrationProposalSpec::from_changeset(
        &change_set,
        TaskStepId::new(format!("step_{id}"))?,
        "snapshot-overlay-base".to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        "scope-shared",
        facts,
    )
}

fn clean_ready_lane_entries(
    plan: &crate::IntegrationPlan,
) -> Result<(Vec<SessionLogEntry>, IntegrationLaneCandidate)> {
    let lane = &plan.lanes[0];
    let candidate = IntegrationLaneCandidate::ManagedRef {
        private_ref: format!(
            "refs/sigil/integration/{}/{}",
            plan.plan_id.as_str(),
            lane.lane_id.as_str()
        ),
        base_commit: "b".repeat(40),
        candidate_commit: "a".repeat(40),
        workspace_snapshot_id: "snapshot-lane-ready".to_owned(),
    };
    let receipts = lane
        .verification_scope_hashes
        .iter()
        .enumerate()
        .map(|(index, scope_hash)| {
            integration_verification_receipt(&format!("promotion-check-{index}"), scope_hash)
        })
        .collect::<Vec<_>>();
    Ok((
        vec![
            SessionLogEntry::Control(ControlEntry::IntegrationPlanRecorded(
                IntegrationPlanRecorded { plan: plan.clone() },
            )),
            SessionLogEntry::Control(ControlEntry::IntegrationLanePrepared(
                IntegrationLanePrepared {
                    plan_id: plan.plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    target: IntegrationLaneTarget::ManagedRef {
                        base_commit: "b".repeat(40),
                        expected_oid: "0".repeat(40),
                        private_ref: format!(
                            "refs/sigil/integration/{}/{}",
                            plan.plan_id.as_str(),
                            lane.lane_id.as_str()
                        ),
                    },
                    owned_workspace_id: "promotion-lane-workspace".to_owned(),
                    ordered_members: lane.proposals.clone(),
                    prepared_at_unix_ms: 1,
                },
            )),
            SessionLogEntry::Control(ControlEntry::IntegrationLaneMemberApplied(
                IntegrationLaneMemberApplied {
                    plan_id: plan.plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    change_set_id: lane.proposals[0].clone(),
                    member_index: 0,
                    effect: IntegrationLaneMemberEffect::ManagedRefAdvanced {
                        expected_old_oid: "0".repeat(40),
                        new_oid: "a".repeat(40),
                        candidate_snapshot_id: "snapshot-lane-ready".to_owned(),
                    },
                    applied_at_unix_ms: 2,
                },
            )),
            SessionLogEntry::Control(ControlEntry::IntegrationLaneVerificationLinked(
                IntegrationLaneVerificationLinked {
                    plan_id: plan.plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    candidate: candidate.clone(),
                    verification_check_ids: receipts
                        .iter()
                        .map(|receipt| receipt.check_spec_id.clone())
                        .collect(),
                    verification_scope_hashes: lane.verification_scope_hashes.clone(),
                    verification_receipts: receipts,
                    linked_at_unix_ms: 3,
                },
            )),
            SessionLogEntry::Control(ControlEntry::IntegrationLaneTerminal(
                IntegrationLaneTerminal {
                    plan_id: plan.plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    status: IntegrationLaneStatus::Ready,
                    candidate: Some(candidate.clone()),
                    reason: None,
                    terminal_at_unix_ms: 4,
                },
            )),
            SessionLogEntry::Control(ControlEntry::IntegrationLaneCleanupRecorded(
                IntegrationLaneCleanupRecorded {
                    plan_id: plan.plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    owned_workspace_id: "promotion-lane-workspace".to_owned(),
                    status: IntegrationLaneCleanupStatus::Removed,
                    recorded_at_unix_ms: 5,
                },
            )),
        ],
        candidate,
    ))
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
                attempt_id: None,
                status: IntegrationPromotionStatus::Promoted,
                preview_digest: "preview-sha256".to_owned(),
                target: IntegrationPromotionTarget::WorkspaceApply {
                    expected_snapshot_id: "snapshot-base".to_owned(),
                    expected_revision: 3,
                },
                authority_nonce: None,
                effect: Some(IntegrationPromotionEffect::WorkspaceApplied {
                    promoted_snapshot_id: "snapshot-parent-after".to_owned(),
                    promoted_revision: 4,
                }),
                recovery_binding: None,
                reason: None,
                recorded_at_unix_ms: 0,
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
fn integration_projection_rebuilds_recovery_critical_lane_lifecycle() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-lifecycle")?,
        TaskId::new("task_lifecycle")?,
        1,
        vec![proposal(
            "change-lifecycle",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let lane = &plan.lanes[0];
    let lane_id = lane.lane_id.clone();
    let candidate = IntegrationLaneCandidate::ManagedRef {
        private_ref: "refs/sigil/integration/plan-lifecycle/lane-1".to_owned(),
        base_commit: "b".repeat(40),
        candidate_commit: "a".repeat(40),
        workspace_snapshot_id: "snapshot-lane".to_owned(),
    };
    let verification_receipts = lane
        .verification_scope_hashes
        .iter()
        .enumerate()
        .map(|(index, scope_hash)| {
            integration_verification_receipt(&format!("git-diff-check-{index}"), scope_hash)
        })
        .collect::<Vec<_>>();
    let verification_check_ids = verification_receipts
        .iter()
        .map(|receipt| receipt.check_spec_id.clone())
        .collect::<Vec<_>>();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::IntegrationPlanRecorded(
            IntegrationPlanRecorded { plan: plan.clone() },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLanePrepared(
            IntegrationLanePrepared {
                plan_id: plan.plan_id.clone(),
                lane_id: lane_id.clone(),
                target: IntegrationLaneTarget::ManagedRef {
                    base_commit: "b".repeat(40),
                    expected_oid: "0".repeat(40),
                    private_ref: "refs/sigil/integration/plan-lifecycle/lane-1".to_owned(),
                },
                owned_workspace_id: "integration-plan-lifecycle-lane-1".to_owned(),
                ordered_members: lane.proposals.clone(),
                prepared_at_unix_ms: 1,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneMemberApplied(
            IntegrationLaneMemberApplied {
                plan_id: plan.plan_id.clone(),
                lane_id: lane_id.clone(),
                change_set_id: lane.proposals[0].clone(),
                member_index: 0,
                effect: IntegrationLaneMemberEffect::ManagedRefAdvanced {
                    expected_old_oid: "0".repeat(40),
                    new_oid: "a".repeat(40),
                    candidate_snapshot_id: "snapshot-lane".to_owned(),
                },
                applied_at_unix_ms: 2,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneVerificationLinked(
            IntegrationLaneVerificationLinked {
                plan_id: plan.plan_id.clone(),
                lane_id: lane_id.clone(),
                candidate: candidate.clone(),
                verification_check_ids,
                verification_scope_hashes: lane.verification_scope_hashes.clone(),
                verification_receipts,
                linked_at_unix_ms: 3,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneTerminal(
            IntegrationLaneTerminal {
                plan_id: plan.plan_id.clone(),
                lane_id: lane_id.clone(),
                status: IntegrationLaneStatus::Ready,
                candidate: Some(candidate),
                reason: None,
                terminal_at_unix_ms: 4,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneCleanupRecorded(
            IntegrationLaneCleanupRecorded {
                plan_id: plan.plan_id.clone(),
                lane_id: lane_id.clone(),
                owned_workspace_id: "integration-plan-lifecycle-lane-1".to_owned(),
                status: IntegrationLaneCleanupStatus::Removed,
                recorded_at_unix_ms: 5,
            },
        )),
    ];

    let projection = IntegrationProjection::from_entries(&entries);
    let state = projection.latest().expect("integration state");
    let lifecycle = state.lifecycle_lanes.get(&lane_id).expect("lane lifecycle");
    assert!(!state.inconsistent);
    assert!(!lifecycle.inconsistent);
    assert_eq!(lifecycle.applied_members.len(), 1);
    assert!(lifecycle.verification.is_some());
    assert_eq!(
        lifecycle.terminal.as_ref().map(|entry| entry.status),
        Some(IntegrationLaneStatus::Ready)
    );
    assert_eq!(
        lifecycle.cleanup.as_ref().map(|entry| entry.status),
        Some(IntegrationLaneCleanupStatus::Removed)
    );
    Ok(())
}

#[test]
fn integration_projection_rebuilds_snapshot_ready_and_conflicted_inventory() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-snapshot-recovery")?,
        TaskId::new("task_snapshot_recovery")?,
        1,
        vec![
            snapshot_proposal("change-ready", "src/ready.rs")?,
            snapshot_proposal("change-conflict", "src/conflict.rs")?,
        ],
    )?;
    let ready_lane = plan
        .lanes
        .iter()
        .find(|lane| lane.proposals[0].as_str() == "change-ready")
        .cloned()
        .expect("ready lane");
    let conflict_lane = plan
        .lanes
        .iter()
        .find(|lane| lane.proposals[0].as_str() == "change-conflict")
        .cloned()
        .expect("conflict lane");
    let ready_candidate = IntegrationLaneCandidate::SnapshotWorkspace {
        owned_workspace_id: "workspace-ready".to_owned(),
        base_snapshot_id: "snapshot-child-ready".to_owned(),
        overlay_digest: format!("sha256:{}", "d".repeat(64)),
        revision: 1,
        candidate_snapshot_id: "snapshot-ready".to_owned(),
    };
    let verification_receipts = ready_lane
        .verification_scope_hashes
        .iter()
        .enumerate()
        .map(|(index, scope_hash)| {
            integration_verification_receipt(&format!("snapshot-check-{index}"), scope_hash)
        })
        .collect::<Vec<_>>();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::IntegrationPlanRecorded(
            IntegrationPlanRecorded { plan: plan.clone() },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLanePrepared(
            IntegrationLanePrepared {
                plan_id: plan.plan_id.clone(),
                lane_id: ready_lane.lane_id.clone(),
                target: IntegrationLaneTarget::SnapshotWorkspace {
                    base_snapshot_id: "snapshot-child-ready".to_owned(),
                    overlay_digest: format!("sha256:{}", "d".repeat(64)),
                    revision: 0,
                    owned_workspace_id: "workspace-ready".to_owned(),
                },
                owned_workspace_id: "workspace-ready".to_owned(),
                ordered_members: ready_lane.proposals.clone(),
                prepared_at_unix_ms: 1,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneMemberApplied(
            IntegrationLaneMemberApplied {
                plan_id: plan.plan_id.clone(),
                lane_id: ready_lane.lane_id.clone(),
                change_set_id: ready_lane.proposals[0].clone(),
                member_index: 0,
                effect: IntegrationLaneMemberEffect::SnapshotWorkspaceApplied {
                    expected_snapshot_id: "snapshot-child-ready".to_owned(),
                    expected_revision: 0,
                    candidate_snapshot_id: "snapshot-ready".to_owned(),
                    candidate_revision: 1,
                },
                applied_at_unix_ms: 2,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneVerificationLinked(
            IntegrationLaneVerificationLinked {
                plan_id: plan.plan_id.clone(),
                lane_id: ready_lane.lane_id.clone(),
                candidate: ready_candidate.clone(),
                verification_check_ids: verification_receipts
                    .iter()
                    .map(|receipt| receipt.check_spec_id.clone())
                    .collect(),
                verification_scope_hashes: ready_lane.verification_scope_hashes.clone(),
                verification_receipts,
                linked_at_unix_ms: 3,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneTerminal(
            IntegrationLaneTerminal {
                plan_id: plan.plan_id.clone(),
                lane_id: ready_lane.lane_id.clone(),
                status: IntegrationLaneStatus::Ready,
                candidate: Some(ready_candidate),
                reason: None,
                terminal_at_unix_ms: 4,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneCleanupRecorded(
            IntegrationLaneCleanupRecorded {
                plan_id: plan.plan_id.clone(),
                lane_id: ready_lane.lane_id.clone(),
                owned_workspace_id: "workspace-ready".to_owned(),
                status: IntegrationLaneCleanupStatus::Retained,
                recorded_at_unix_ms: 5,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLanePrepared(
            IntegrationLanePrepared {
                plan_id: plan.plan_id.clone(),
                lane_id: conflict_lane.lane_id.clone(),
                target: IntegrationLaneTarget::SnapshotWorkspace {
                    base_snapshot_id: "snapshot-child-conflict".to_owned(),
                    overlay_digest: format!("sha256:{}", "d".repeat(64)),
                    revision: 0,
                    owned_workspace_id: "workspace-conflict".to_owned(),
                },
                owned_workspace_id: "workspace-conflict".to_owned(),
                ordered_members: conflict_lane.proposals.clone(),
                prepared_at_unix_ms: 6,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneTerminal(
            IntegrationLaneTerminal {
                plan_id: plan.plan_id.clone(),
                lane_id: conflict_lane.lane_id.clone(),
                status: IntegrationLaneStatus::Conflict,
                candidate: None,
                reason: Some("content conflict".to_owned()),
                terminal_at_unix_ms: 7,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneCleanupRecorded(
            IntegrationLaneCleanupRecorded {
                plan_id: plan.plan_id,
                lane_id: conflict_lane.lane_id.clone(),
                owned_workspace_id: "workspace-conflict".to_owned(),
                status: IntegrationLaneCleanupStatus::Removed,
                recorded_at_unix_ms: 8,
            },
        )),
    ];

    let projection = IntegrationProjection::from_entries(&entries);
    let state = projection.latest().expect("snapshot recovery state");
    assert!(!state.inconsistent);
    let ready = state
        .lifecycle_lanes
        .get(&ready_lane.lane_id)
        .expect("ready snapshot lifecycle");
    assert!(!ready.inconsistent);
    assert!(ready.verification.is_some());
    assert_eq!(
        ready.cleanup.as_ref().map(|entry| entry.status),
        Some(IntegrationLaneCleanupStatus::Retained)
    );
    let conflict = state
        .lifecycle_lanes
        .get(&conflict_lane.lane_id)
        .expect("conflicted snapshot lifecycle");
    assert!(!conflict.inconsistent);
    assert!(conflict.applied_members.is_empty());
    assert_eq!(
        conflict.terminal.as_ref().map(|entry| entry.status),
        Some(IntegrationLaneStatus::Conflict)
    );
    assert_eq!(
        conflict.cleanup.as_ref().map(|entry| entry.status),
        Some(IntegrationLaneCleanupStatus::Removed)
    );
    Ok(())
}

#[test]
fn integration_projection_rejects_out_of_order_lane_member_receipt() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-member-order")?,
        TaskId::new("task_member_order")?,
        1,
        vec![proposal(
            "change-member-order",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let lane = &plan.lanes[0];
    let lane_id = lane.lane_id.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::IntegrationPlanRecorded(
            IntegrationPlanRecorded { plan: plan.clone() },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLanePrepared(
            IntegrationLanePrepared {
                plan_id: plan.plan_id.clone(),
                lane_id: lane_id.clone(),
                target: IntegrationLaneTarget::ManagedRef {
                    base_commit: "b".repeat(40),
                    expected_oid: "0".repeat(40),
                    private_ref: "refs/sigil/integration/plan-member-order/lane-1".to_owned(),
                },
                owned_workspace_id: "integration-plan-member-order-lane-1".to_owned(),
                ordered_members: lane.proposals.clone(),
                prepared_at_unix_ms: 1,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationLaneMemberApplied(
            IntegrationLaneMemberApplied {
                plan_id: plan.plan_id,
                lane_id,
                change_set_id: lane.proposals[0].clone(),
                member_index: 1,
                effect: IntegrationLaneMemberEffect::ManagedRefAdvanced {
                    expected_old_oid: "0".repeat(40),
                    new_oid: "a".repeat(40),
                    candidate_snapshot_id: "snapshot-lane".to_owned(),
                },
                applied_at_unix_ms: 2,
            },
        )),
    ];

    assert!(
        IntegrationProjection::from_entries(&entries)
            .latest()
            .is_some_and(|state| state.inconsistent)
    );
    Ok(())
}

#[test]
fn promotion_preview_requires_ready_lanes_and_rejects_executable_intent_ref_target() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-promotion-preview")?,
        TaskId::new("task_promotion_preview")?,
        1,
        vec![proposal(
            "change-promotion-preview",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let pending_entries = vec![SessionLogEntry::Control(
        ControlEntry::IntegrationPlanRecorded(IntegrationPlanRecorded { plan: plan.clone() }),
    )];
    let pending = IntegrationProjection::from_entries(&pending_entries);
    let input = TaskPromotionPreviewInput {
        aggregate_diff_artifact_ref: "artifact-aggregate".to_owned(),
        aggregate_diff_digest: format!("sha256:{}", "a".repeat(64)),
        target: IntegrationPromotionTarget::WorkspaceApply {
            expected_snapshot_id: plan.base_snapshot_id.clone(),
            expected_revision: 0,
        },
        verification_invalidation: vec!["scope-shared".to_owned()],
        intent_binding: None,
        policy_digest: VerificationPolicy::no_checks_required("scope-shared").stable_hash()?,
        has_pending_approval: false,
        has_executable_intent_refs: false,
        created_at_unix_ms: 10,
    };
    assert!(
        build_task_promotion_preview(pending.latest().expect("pending plan"), input.clone())
            .is_err()
    );

    let (ready_entries, _) = clean_ready_lane_entries(&plan)?;
    let ready = IntegrationProjection::from_entries(&ready_entries);
    assert!(
        ready
            .latest()
            .expect("ready lanes")
            .synthesis_ready_attempt()
            .is_none()
    );
    let preview = build_task_promotion_preview(ready.latest().expect("ready plan"), input.clone())?;
    preview.validate()?;
    assert_eq!(preview.ordered_lane_candidates.len(), 1);
    assert!(preview.policy_digest.starts_with("sha256:jcs-v1:"));

    let mut ref_input = input;
    ref_input.target = IntegrationPromotionTarget::GitRefAdvance {
        target_ref: "refs/heads/integration-target".to_owned(),
        expected_old_oid: "b".repeat(40),
        candidate_oid: "a".repeat(40),
    };
    ref_input.has_executable_intent_refs = true;
    assert!(build_task_promotion_preview(ready.latest().expect("ready plan"), ref_input).is_err());
    Ok(())
}

#[test]
fn integration_review_product_is_exact_and_suppresses_consumed_or_superseded_previews() -> Result<()>
{
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-review-product")?,
        TaskId::new("task_review_product")?,
        1,
        vec![proposal(
            "change-review-product",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let (mut entries, _) = clean_ready_lane_entries(&plan)?;
    let ready = IntegrationProjection::from_entries(&entries);
    let preview = build_task_promotion_preview(
        ready.latest().expect("ready review plan"),
        TaskPromotionPreviewInput {
            aggregate_diff_artifact_ref: "artifact-review-product".to_owned(),
            aggregate_diff_digest: format!("sha256:{}", "a".repeat(64)),
            target: IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id: plan.base_snapshot_id.clone(),
                expected_revision: 0,
            },
            verification_invalidation: vec!["scope-shared".to_owned()],
            intent_binding: None,
            policy_digest: VerificationPolicy::no_checks_required("scope-shared").stable_hash()?,
            has_pending_approval: false,
            has_executable_intent_refs: false,
            created_at_unix_ms: 10,
        },
    )?;
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionPreviewRecorded(TaskPromotionPreviewRecorded {
            preview: preview.clone(),
        }),
    ));

    let product =
        crate::task_integration_review_product(&entries).expect("current preview review product");
    assert_eq!(product.preview, preview);
    product.request.validate_for_preview(&product.preview)?;

    let authority = TaskPromotionAuthority::from_user_integration_review(
        &product.preview,
        product.request.request_id.clone(),
        100,
        "nonce-review-product",
    )?;
    let mut consumed_entries = entries.clone();
    consumed_entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionAuthorityConsumed(TaskPromotionAuthorityConsumed {
            attempt_id: IntegrationPromotionAttemptId::new("attempt-review-product")?,
            authority,
            consumed_at_unix_ms: 20,
        }),
    ));
    assert!(crate::task_integration_review_product(&consumed_entries).is_none());

    let mut superseded_entries = entries;
    let mut superseding = plan.clone();
    superseding.plan_id = IntegrationPlanId::new("plan-review-product-v2")?;
    superseding.plan_version = 2;
    superseded_entries.push(SessionLogEntry::Control(
        ControlEntry::IntegrationPlanRecorded(IntegrationPlanRecorded { plan: superseding }),
    ));
    assert!(crate::task_integration_review_product(&superseded_entries).is_none());
    Ok(())
}

#[test]
fn promotion_authority_replay_and_target_mismatch_are_rejected_before_effect() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-promotion-authority")?,
        TaskId::new("task_promotion_authority")?,
        1,
        vec![proposal(
            "change-promotion-authority",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let (mut entries, _) = clean_ready_lane_entries(&plan)?;
    let ready = IntegrationProjection::from_entries(&entries);
    let preview = build_task_promotion_preview(
        ready.latest().expect("ready plan"),
        TaskPromotionPreviewInput {
            aggregate_diff_artifact_ref: "artifact-authority".to_owned(),
            aggregate_diff_digest: format!("sha256:{}", "a".repeat(64)),
            target: IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id: plan.base_snapshot_id.clone(),
                expected_revision: 0,
            },
            verification_invalidation: vec!["scope-shared".to_owned()],
            intent_binding: None,
            policy_digest: format!("sha256:{}", "c".repeat(64)),
            has_pending_approval: false,
            has_executable_intent_refs: false,
            created_at_unix_ms: 10,
        },
    )?;
    let authority = TaskPromotionAuthority::from_user_integration_review(
        &preview,
        "review-authority",
        100,
        "nonce-authority",
    )?;
    let consumed = TaskPromotionAuthorityConsumed {
        attempt_id: IntegrationPromotionAttemptId::new("promotion-attempt-authority")?,
        authority: authority.clone(),
        consumed_at_unix_ms: 20,
    };
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionPreviewRecorded(TaskPromotionPreviewRecorded {
            preview: preview.clone(),
        }),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionAuthorityConsumed(consumed.clone()),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionAuthorityConsumed(consumed),
    ));
    let replayed = IntegrationProjection::from_entries(&entries);
    let replayed = replayed.latest().expect("replayed authority");
    assert!(replayed.inconsistent);
    assert!(replayed.promotions.is_empty());

    let (mut mismatched_entries, _) = clean_ready_lane_entries(&plan)?;
    mismatched_entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionPreviewRecorded(TaskPromotionPreviewRecorded {
            preview: preview.clone(),
        }),
    ));
    let mut mismatched = authority;
    mismatched.target = IntegrationPromotionTarget::WorkspaceApply {
        expected_snapshot_id: plan.base_snapshot_id,
        expected_revision: 1,
    };
    mismatched_entries.push(SessionLogEntry::Control(
        ControlEntry::TaskPromotionAuthorityConsumed(TaskPromotionAuthorityConsumed {
            attempt_id: IntegrationPromotionAttemptId::new("promotion-attempt-mismatch")?,
            authority: mismatched,
            consumed_at_unix_ms: 20,
        }),
    ));
    let mismatched = IntegrationProjection::from_entries(&mismatched_entries);
    let mismatched = mismatched.latest().expect("mismatched authority");
    assert!(mismatched.inconsistent);
    assert!(mismatched.promotions.is_empty());
    Ok(())
}

#[test]
fn promotion_protocol_replays_single_target_effect_and_parent_verification() -> Result<()> {
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-promotion-protocol")?,
        TaskId::new("task_promotion_protocol")?,
        1,
        vec![proposal(
            "change-promotion-protocol",
            &["src/lib.rs"],
            &[],
            &[],
            IntegrationEffect::Files,
        )?],
    )?;
    let (mut entries, _) = clean_ready_lane_entries(&plan)?;
    let ready = IntegrationProjection::from_entries(&entries);
    let preview = build_task_promotion_preview(
        ready.latest().expect("ready plan"),
        TaskPromotionPreviewInput {
            aggregate_diff_artifact_ref: "artifact-protocol".to_owned(),
            aggregate_diff_digest: format!("sha256:{}", "a".repeat(64)),
            target: IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id: plan.base_snapshot_id.clone(),
                expected_revision: 0,
            },
            verification_invalidation: vec!["scope-shared".to_owned()],
            intent_binding: Some("intent-binding".to_owned()),
            policy_digest: format!("sha256:{}", "c".repeat(64)),
            has_pending_approval: false,
            has_executable_intent_refs: false,
            created_at_unix_ms: 10,
        },
    )?;
    let authority = TaskPromotionAuthority::from_user_integration_review(
        &preview,
        "review-protocol",
        100,
        "nonce-protocol",
    )?;
    let attempt_id = IntegrationPromotionAttemptId::new("promotion-attempt-protocol")?;
    let recovery_binding = IntegrationPromotionRecoveryBinding {
        owned_workspace_id: "promotion-protocol-workspace".to_owned(),
        candidate_snapshot_id: "snapshot-promotion-candidate".to_owned(),
        expected_parent_snapshot_id: Some("snapshot-expected-parent".to_owned()),
    };
    entries.extend([
        SessionLogEntry::Control(ControlEntry::TaskPromotionPreviewRecorded(
            TaskPromotionPreviewRecorded {
                preview: preview.clone(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskPromotionAuthorityConsumed(
            TaskPromotionAuthorityConsumed {
                attempt_id: attempt_id.clone(),
                authority: authority.clone(),
                consumed_at_unix_ms: 20,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationPromotionRecorded(
            IntegrationPromotionRecorded {
                plan_id: plan.plan_id.clone(),
                attempt_id: Some(attempt_id.clone()),
                status: IntegrationPromotionStatus::Prepared,
                preview_digest: preview.preview_digest.clone(),
                target: preview.target.clone(),
                authority_nonce: Some(authority.nonce.clone()),
                effect: None,
                recovery_binding: Some(recovery_binding.clone()),
                reason: None,
                recorded_at_unix_ms: 21,
            },
        )),
        SessionLogEntry::Control(ControlEntry::IntegrationPromotionRecorded(
            IntegrationPromotionRecorded {
                plan_id: plan.plan_id.clone(),
                attempt_id: Some(attempt_id.clone()),
                status: IntegrationPromotionStatus::Promoted,
                preview_digest: preview.preview_digest.clone(),
                target: preview.target.clone(),
                authority_nonce: Some(authority.nonce),
                effect: Some(IntegrationPromotionEffect::WorkspaceApplied {
                    promoted_snapshot_id: "snapshot-parent-promoted".to_owned(),
                    promoted_revision: 1,
                }),
                recovery_binding: Some(recovery_binding),
                reason: None,
                recorded_at_unix_ms: 22,
            },
        )),
    ]);
    let mut mismatched_recovery_entries = entries.clone();
    let Some(SessionLogEntry::Control(ControlEntry::IntegrationPromotionRecorded(terminal))) =
        mismatched_recovery_entries.last_mut()
    else {
        panic!("terminal promotion entry");
    };
    terminal
        .recovery_binding
        .as_mut()
        .expect("terminal recovery binding")
        .candidate_snapshot_id = "snapshot-substituted-candidate".to_owned();
    assert!(
        IntegrationProjection::from_entries(&mismatched_recovery_entries)
            .latest()
            .expect("mismatched recovery projection")
            .inconsistent
    );
    let mut receipt = integration_verification_receipt("parent-check", "scope-parent");
    receipt.binding.workspace_snapshot_id = "snapshot-parent-promoted".to_owned();
    receipt.receipt.workspace_snapshot_id = Some("snapshot-parent-promoted".to_owned());
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskParentVerificationRecorded(TaskParentVerificationRecorded {
            attempt_id: attempt_id.clone(),
            plan_id: plan.plan_id,
            preview_digest: preview.preview_digest,
            promoted_snapshot_id: "snapshot-parent-promoted".to_owned(),
            policy_digest: preview.policy_digest,
            verdict: VerificationVerdict::Passed,
            receipts: vec![receipt],
            reason: None,
            recorded_at_unix_ms: 23,
        }),
    ));

    let projection = IntegrationProjection::from_entries(&entries);
    let state = projection.latest().expect("promotion protocol state");
    assert!(!state.inconsistent);
    assert_eq!(state.promotions.len(), 2);
    assert_eq!(
        state
            .parent_verifications
            .get(&attempt_id)
            .map(|verification| verification.verdict),
        Some(VerificationVerdict::Passed)
    );
    assert_eq!(state.synthesis_ready_attempt(), Some(&attempt_id));
    Ok(())
}

#[test]
fn not_applicable_parent_verification_requires_zero_check_receipts() -> Result<()> {
    let mut record = TaskParentVerificationRecorded {
        attempt_id: IntegrationPromotionAttemptId::new("attempt-no-parent-checks")?,
        plan_id: IntegrationPlanId::new("plan-no-parent-checks")?,
        preview_digest: format!("sha256:{}", "a".repeat(64)),
        promoted_snapshot_id: "snapshot-no-parent-checks".to_owned(),
        policy_digest: VerificationPolicy::no_checks_required("scope-no-parent-checks")
            .stable_hash()?,
        verdict: VerificationVerdict::NotApplicable,
        receipts: Vec::new(),
        reason: Some("accepted parent verification policy requires no checks".to_owned()),
        recorded_at_unix_ms: 1,
    };
    record.validate()?;

    let mut receipt =
        integration_verification_receipt("unexpected-check", "scope-no-parent-checks");
    receipt.binding.workspace_snapshot_id = record.promoted_snapshot_id.clone();
    receipt.receipt.workspace_snapshot_id = Some(record.promoted_snapshot_id.clone());
    record.receipts.push(receipt);
    assert!(record.validate().is_err());
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
                attempt_id: None,
                status: IntegrationPromotionStatus::Promoted,
                preview_digest: "preview-sha256".to_owned(),
                target: IntegrationPromotionTarget::WorkspaceApply {
                    expected_snapshot_id: "snapshot-base".to_owned(),
                    expected_revision: 3,
                },
                authority_nonce: None,
                effect: Some(IntegrationPromotionEffect::GitRefAdvanced {
                    old_oid: "b".repeat(40),
                    new_oid: "a".repeat(40),
                }),
                recovery_binding: None,
                reason: None,
                recorded_at_unix_ms: 0,
            },
        )),
    ];

    let projection = IntegrationProjection::from_entries(&entries);
    let state = projection.latest().expect("integration state");
    assert!(state.inconsistent);
    Ok(())
}
