use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    CandidateCheck, ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk,
    CheckCommand, CheckDiscoverySource, CheckPromotion, CompletionCriteria, ControlEntry,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, EvidenceScope, ExecutionBackend,
    ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionFuture, ExecutionNetworkReceipt,
    ExecutionRequest, IntegrationBaseRepresentation, IntegrationContentClass, IntegrationEffect,
    IntegrationLaneCandidate, IntegrationLaneCleanupStatus, IntegrationLaneStatus, IntegrationPlan,
    IntegrationPlanId, IntegrationPlanRecorded, IntegrationProjection,
    IntegrationPromotionAttemptId, IntegrationPromotionEffect, IntegrationPromotionStatus,
    IntegrationProposalFacts, IntegrationProposalSpec, JsonlSessionStore, MutationEventRecorder,
    NoopEventHandler, SandboxProfileRequirement, Session, SessionLogEntry, TaskId,
    TaskPromotionAuthority, TaskPromotionPreview, TaskPromotionPreviewInput,
    TaskPromotionPreviewRecorded, TaskStepId, ToolEffect, TrustedCheckSpec,
    VerificationAutoRunPolicy, VerificationPolicy, VerificationPolicyChangedEntry,
    VerificationScope, VerificationVerdict, WorkspaceTrust, WorkspaceTrustRequirement,
    build_integration_plan, build_task_promotion_preview, build_workspace_snapshot,
    stable_workspace_id,
};
use sigil_tools_builtin::LocalExecutionBackend;

use super::{
    GitIntegrationPromotionPreparationRequest, GitIntegrationPromotionRunRequest,
    GitIntegrationRunRequest, IntegrationArtifact, IntegrationLaneRuntimeEvent,
    IntegrationLaneRuntimeEventRequest, IntegrationPromotionPreparationTarget,
    IntegrationPromotionRuntimeEvent, IntegrationPromotionRuntimeEventRequest,
    ParentVerificationRunRequest, prepare_git_integration_promotion,
    reconcile_integration_promotions, run_authoritative_parent_verification,
    run_git_integration_lanes, run_git_integration_lanes_with_events,
    run_git_integration_promotion_with_events,
};
use crate::isolated_workspace::{
    GitWorktreeBaseFreezeRequest, GitWorktreeCleanupRequest, cleanup_git_worktree,
    freeze_git_worktree_base,
};

struct BarrierVerificationBackend {
    barrier: Arc<tokio::sync::Barrier>,
    inner: LocalExecutionBackend,
}

impl ExecutionBackend for BarrierVerificationBackend {
    fn kind(&self) -> ExecutionBackendKind {
        self.inner.kind()
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        self.inner.capabilities()
    }

    fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
        self.inner.planned_network_receipt()
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        Box::pin(async move {
            self.barrier.wait().await;
            self.inner.execute(request).await
        })
    }
}

#[tokio::test]
async fn disjoint_git_integration_lanes_overlap_and_preserve_parent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n"), ("b.txt", "old-b\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let change_a = change_set("change-a", "a.txt", "old-a\n", "new-a\n")?;
    let change_b = change_set("change-b", "b.txt", "old-b\n", "new-b\n")?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-parallel")?,
        TaskId::new("task_parallel")?,
        1,
        vec![
            proposal(&change_a, "step_a", &base_snapshot_id, &base_commit)?,
            proposal(&change_b, "step_b", &base_snapshot_id, &base_commit)?,
        ],
    )?;
    assert_eq!(plan.lanes.len(), 2);

    let verification_backend = Arc::new(BarrierVerificationBackend {
        barrier: Arc::new(tokio::sync::Barrier::new(2)),
        inner: LocalExecutionBackend,
    });
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        run_git_integration_lanes(GitIntegrationRunRequest {
            parent_workspace_root: root.clone(),
            plan,
            artifacts: vec![
                artifact(
                    change_a,
                    "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
                ),
                artifact(
                    change_b,
                    "--- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-old-b\n+new-b\n",
                ),
            ],
            frozen_base: None,
            verification_backend: Some(verification_backend),
        }),
    )
    .await
    .context("independent integration lane checks did not overlap")??;

    assert_eq!(output.lanes.len(), 2);
    assert!(
        output
            .lanes
            .iter()
            .all(|lane| lane.status == IntegrationLaneStatus::Ready)
    );
    assert!(output.lanes.iter().all(|lane| lane.cleanup_error.is_none()));
    let latest_start = output
        .lanes
        .iter()
        .map(|lane| lane.started_at_unix_ms)
        .max()
        .expect("lane start");
    let earliest_finish = output
        .lanes
        .iter()
        .map(|lane| lane.finished_at_unix_ms)
        .min()
        .expect("lane finish");
    assert!(
        latest_start <= earliest_finish,
        "lane execution intervals should overlap"
    );
    assert_eq!(std::fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    assert_eq!(std::fs::read_to_string(root.join("b.txt"))?, "old-b\n");
    for lane in &output.lanes {
        let IntegrationLaneCandidate::ManagedRef {
            private_ref,
            candidate_commit,
            ..
        } = lane.candidate.as_ref().expect("integration candidate")
        else {
            panic!("clean integration lane should produce a managed ref");
        };
        let reference = private_ref.as_str();
        let commit = git(&root, &["rev-parse", "--verify", reference])?;
        assert_eq!(&commit, candidate_commit);
    }
    assert_eq!(
        git(
            &root,
            &["show", "refs/sigil/integration/plan-parallel/lane-1:a.txt"]
        )?,
        "new-a"
    );
    assert_eq!(
        git(
            &root,
            &["show", "refs/sigil/integration/plan-parallel/lane-2:b.txt"]
        )?,
        "new-b"
    );
    Ok(())
}

#[tokio::test]
async fn dependent_lane_applies_members_in_stable_plan_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n"), ("b.txt", "old-b\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let change_a = change_set("change-order-a", "a.txt", "old-a\n", "new-a\n")?;
    let change_b = change_set("change-order-b", "b.txt", "old-b\n", "new-b\n")?;
    let proposal_a = proposal(&change_a, "step_order_a", &base_snapshot_id, &base_commit)?;
    let mut proposal_b = proposal(&change_b, "step_order_b", &base_snapshot_id, &base_commit)?;
    proposal_b.depends_on = vec![change_a.id.clone()];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-member-order")?,
        TaskId::new("task_member_order")?,
        1,
        vec![proposal_b, proposal_a],
    )?;
    assert_eq!(plan.lanes.len(), 1);
    assert_eq!(
        plan.lanes[0].proposals,
        vec![change_a.id.clone(), change_b.id.clone()]
    );
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<IntegrationLaneRuntimeEventRequest>();
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(request) = event_rx.recv().await {
            events.push(request.event().clone());
            request.acknowledge(Ok(()));
        }
        events
    });
    let output = run_git_integration_lanes_with_events(
        GitIntegrationRunRequest {
            parent_workspace_root: root.clone(),
            plan,
            artifacts: vec![
                artifact(
                    change_b,
                    "--- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-old-b\n+new-b\n",
                ),
                artifact(
                    change_a,
                    "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
                ),
            ],
            frozen_base: None,
            verification_backend: None,
        },
        Some(event_tx.clone()),
    )
    .await?;
    drop(event_tx);
    let events = collector.await?;
    assert_eq!(output.lanes[0].status, IntegrationLaneStatus::Ready);
    let applied = events
        .iter()
        .filter_map(|event| match event {
            IntegrationLaneRuntimeEvent::MemberApplied(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(applied.len(), 2);
    assert_eq!(applied[0].member_index, 0);
    assert_eq!(applied[0].change_set_id.as_str(), "change-order-a");
    assert_eq!(applied[1].member_index, 1);
    assert_eq!(applied[1].change_set_id.as_str(), "change-order-b");
    assert_eq!(fs::read(root.join("a.txt"))?, b"old-a\n");
    assert_eq!(fs::read(root.join("b.txt"))?, b"old-b\n");
    Ok(())
}

#[tokio::test]
async fn conflicting_lane_retains_partial_private_ref_without_parent_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("shared.txt", "old\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let first = change_set("change-first", "shared.txt", "old\n", "first\n")?;
    let second = change_set("change-second", "shared.txt", "old\n", "second\n")?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-conflict")?,
        TaskId::new("task_conflict")?,
        1,
        vec![
            proposal(&first, "step_first", &base_snapshot_id, &base_commit)?,
            proposal(&second, "step_second", &base_snapshot_id, &base_commit)?,
        ],
    )?;
    assert_eq!(plan.lanes.len(), 1);

    let output = run_git_integration_lanes(GitIntegrationRunRequest {
        parent_workspace_root: root.clone(),
        plan,
        artifacts: vec![
            artifact(
                first,
                "--- a/shared.txt\n+++ b/shared.txt\n@@ -1,1 +1,1 @@\n-old\n+first\n",
            ),
            artifact(
                second,
                "--- a/shared.txt\n+++ b/shared.txt\n@@ -1,1 +1,1 @@\n-old\n+second\n",
            ),
        ],
        frozen_base: None,
        verification_backend: None,
    })
    .await?;

    assert_eq!(output.lanes.len(), 1);
    assert_eq!(output.lanes[0].status, IntegrationLaneStatus::Conflict);
    assert!(output.lanes[0].candidate.is_none());
    assert_eq!(std::fs::read_to_string(root.join("shared.txt"))?, "old\n");
    assert!(git_ref_exists(
        &root,
        "refs/sigil/integration/plan-conflict/lane-1"
    )?);
    assert_eq!(
        git(
            &root,
            &[
                "show",
                "refs/sigil/integration/plan-conflict/lane-1:shared.txt"
            ]
        )?,
        "first"
    );
    Ok(())
}

#[tokio::test]
async fn overlay_plan_is_rejected_before_managed_ref_materialization() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let change = change_set("change-overlay", "a.txt", "old-a\n", "new-a\n")?;
    let mut proposal = proposal(&change, "step_overlay", &base_snapshot_id, &base_commit)?;
    proposal.facts.base_representation = IntegrationBaseRepresentation::SnapshotWorkspace {
        base_commit: git(&root, &["rev-parse", "HEAD"])?,
        overlay_digest: format!("sha256:{}", "a".repeat(64)),
    };
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-overlay")?,
        TaskId::new("task_overlay")?,
        1,
        vec![proposal],
    )?;
    let error = run_git_integration_lanes(GitIntegrationRunRequest {
        parent_workspace_root: root.clone(),
        plan,
        artifacts: vec![artifact(
            change,
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
        )],
        frozen_base: None,
        verification_backend: None,
    })
    .await
    .expect_err("overlay plans must not enter managed-ref integration");

    assert!(
        error
            .to_string()
            .contains("snapshot integration requires a frozen overlay base")
    );
    assert_eq!(std::fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    assert!(!git_ref_exists(
        &root,
        "refs/sigil/integration/plan-overlay/lane-1"
    )?);
    Ok(())
}

#[tokio::test]
async fn managed_ref_plan_rejects_base_commit_drift_before_ref_creation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let change = change_set("change-stale-base", "a.txt", "old-a\n", "new-a\n")?;
    let proposal = proposal(
        &change,
        "step_stale_base",
        &base_snapshot_id,
        &"a".repeat(40),
    )?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-stale-base")?,
        TaskId::new("task_stale_base")?,
        1,
        vec![proposal],
    )?;
    let workspace_id = super::git_integration_workspace_id(&plan.plan_id, &plan.lanes[0].lane_id);
    let error = run_git_integration_lanes(GitIntegrationRunRequest {
        parent_workspace_root: root.clone(),
        plan,
        artifacts: vec![artifact(
            change,
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
        )],
        frozen_base: None,
        verification_backend: None,
    })
    .await
    .expect_err("base commit drift must reject managed-ref integration");

    assert!(
        error
            .to_string()
            .contains("integration base commit drifted")
    );
    assert_eq!(std::fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    assert!(!git_ref_exists(
        &root,
        "refs/sigil/integration/plan-stale-base/lane-1"
    )?);
    assert!(
        !root
            .join(".git/sigil-isolated-worktrees")
            .join(workspace_id)
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn snapshot_workspace_lanes_overlap_preserve_overlay_and_emit_recovery_facts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(
        &root,
        &[
            ("a.txt", "base-a\n"),
            ("b.txt", "old-b\n"),
            ("c.txt", "old-c\n"),
        ],
    )?;
    fs::write(root.join("a.txt"), "user-a\n")?;
    fs::write(root.join("notes.txt"), "user-notes\n")?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let recorder = MutationEventRecorder::new(JsonlSessionStore::new(
        temp.path().join("integration-session.jsonl"),
    )?);
    let frozen = freeze_git_worktree_base(GitWorktreeBaseFreezeRequest {
        parent_workspace_root: root.clone(),
        base_snapshot_id: base_snapshot_id.clone(),
        operation_id: "snapshot-integration".to_owned(),
        artifact_recorder: recorder,
    })
    .await?;
    let base_representation = IntegrationBaseRepresentation::SnapshotWorkspace {
        base_commit: frozen.base_commit().to_owned(),
        overlay_digest: frozen.overlay_digest().to_owned(),
    };
    let change_b = change_set("change-snapshot-b", "b.txt", "old-b\n", "new-b\n")?;
    let change_c = change_set("change-snapshot-c", "c.txt", "old-c\n", "new-c\n")?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-snapshot")?,
        TaskId::new("task_snapshot")?,
        1,
        vec![
            proposal_with_base(
                &change_b,
                "step_snapshot_b",
                &base_snapshot_id,
                base_representation.clone(),
            )?,
            proposal_with_base(
                &change_c,
                "step_snapshot_c",
                &base_snapshot_id,
                base_representation,
            )?,
        ],
    )?;
    assert_eq!(plan.lanes.len(), 2);
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<IntegrationLaneRuntimeEventRequest>();
    let recorded_events = Arc::new(Mutex::new(Vec::new()));
    let event_collector = Arc::clone(&recorded_events);
    let collector = tokio::spawn(async move {
        while let Some(request) = event_rx.recv().await {
            event_collector
                .lock()
                .expect("event collector lock")
                .push(request.event().clone());
            request.acknowledge(Ok(()));
        }
    });
    let output = run_git_integration_lanes_with_events(
        GitIntegrationRunRequest {
            parent_workspace_root: root.clone(),
            plan,
            artifacts: vec![
                artifact(
                    change_b,
                    "--- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-old-b\n+new-b\n",
                ),
                artifact(
                    change_c,
                    "--- a/c.txt\n+++ b/c.txt\n@@ -1,1 +1,1 @@\n-old-c\n+new-c\n",
                ),
            ],
            frozen_base: Some(frozen),
            verification_backend: None,
        },
        Some(event_tx.clone()),
    )
    .await?;
    drop(event_tx);
    collector.await?;
    let events = recorded_events.lock().expect("recorded event lock").clone();

    assert_eq!(output.lanes.len(), 2);
    assert!(
        output
            .lanes
            .iter()
            .all(|lane| lane.status == IntegrationLaneStatus::Ready)
    );
    let latest_start = output
        .lanes
        .iter()
        .map(|lane| lane.started_at_unix_ms)
        .max()
        .expect("lane start");
    let earliest_finish = output
        .lanes
        .iter()
        .map(|lane| lane.finished_at_unix_ms)
        .min()
        .expect("lane finish");
    assert!(latest_start <= earliest_finish);
    assert_eq!(fs::read(root.join("a.txt"))?, b"user-a\n");
    assert_eq!(fs::read(root.join("b.txt"))?, b"old-b\n");
    assert_eq!(fs::read(root.join("c.txt"))?, b"old-c\n");
    assert_eq!(fs::read(root.join("notes.txt"))?, b"user-notes\n");

    let git_common_dir = root.join(git(&root, &["rev-parse", "--git-common-dir"])?);
    for lane in &output.lanes {
        let IntegrationLaneCandidate::SnapshotWorkspace {
            owned_workspace_id,
            revision,
            ..
        } = lane.candidate.as_ref().expect("snapshot candidate")
        else {
            panic!("overlay integration must produce a snapshot workspace");
        };
        assert_eq!(*revision, 1);
        let workspace = git_common_dir
            .join("sigil-isolated-worktrees")
            .join(owned_workspace_id);
        assert_eq!(fs::read(workspace.join("a.txt"))?, b"user-a\n");
        assert_eq!(fs::read(workspace.join("notes.txt"))?, b"user-notes\n");
        if lane.lane_id.as_str() == "lane-1" {
            assert_eq!(fs::read(workspace.join("b.txt"))?, b"new-b\n");
            assert_eq!(fs::read(workspace.join("c.txt"))?, b"old-c\n");
        } else {
            assert_eq!(fs::read(workspace.join("b.txt"))?, b"old-b\n");
            assert_eq!(fs::read(workspace.join("c.txt"))?, b"new-c\n");
        }
        cleanup_git_worktree(GitWorktreeCleanupRequest {
            parent_workspace_root: root.clone(),
            isolated_workspace_id: owned_workspace_id.clone(),
        })
        .await?;
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, IntegrationLaneRuntimeEvent::Prepared { .. }))
            .count(),
        2
    );
    for verification in events.iter().filter_map(|event| match event {
        IntegrationLaneRuntimeEvent::VerificationLinked(verification) => Some(verification),
        _ => None,
    }) {
        assert_eq!(verification.verification_receipts.len(), 1);
        let receipt = &verification.verification_receipts[0];
        assert_eq!(receipt.check_status, sigil_kernel::ReceiptStatus::Succeeded);
        assert_eq!(
            receipt.binding.execution_backend,
            Some(sigil_kernel::ExecutionBackendKind::Local)
        );
        assert_eq!(
            receipt.binding.execution_network.policy,
            sigil_kernel::ExecutionNetworkPolicy::Unknown
        );
        assert_eq!(
            receipt.binding.verification_scope_hash,
            DEFAULT_TASK_VERIFICATION_SCOPE_HASH
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, IntegrationLaneRuntimeEvent::MemberApplied(_)))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, IntegrationLaneRuntimeEvent::VerificationLinked(_)))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, IntegrationLaneRuntimeEvent::Terminal(_)))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                IntegrationLaneRuntimeEvent::CleanupRecorded { entry, .. }
                    if entry.status == IntegrationLaneCleanupStatus::Retained
            ))
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn workspace_promotion_applies_aggregate_batch_after_authority_barrier() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n"), ("b.txt", "old-b\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let change_a = change_set("promotion-a", "a.txt", "old-a\n", "new-a\n")?;
    let change_b = change_set("promotion-b", "b.txt", "old-b\n", "new-b\n")?;
    let artifacts = vec![
        artifact(
            change_a.clone(),
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
        ),
        artifact(
            change_b.clone(),
            "--- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-old-b\n+new-b\n",
        ),
    ];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-workspace-promotion")?,
        TaskId::new("task_workspace_promotion")?,
        1,
        vec![
            proposal(
                &change_a,
                "step_promotion_a",
                &base_snapshot_id,
                &base_commit,
            )?,
            proposal(
                &change_b,
                "step_promotion_b",
                &base_snapshot_id,
                &base_commit,
            )?,
        ],
    )?;
    let lane_session = ready_lane_session(&root, &plan, &artifacts).await?;
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: "workspace-review".to_owned(),
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: IntegrationPromotionPreparationTarget::WorkspaceApply {
            expected_snapshot_id: base_snapshot_id,
            expected_revision: 0,
        },
    })
    .await?;
    let preview = promotion_preview(&lane_session, &plan, &prepared)?;
    let authority = promotion_authority(&preview, "workspace-review")?;
    let mutation_store = JsonlSessionStore::new(temp.path().join("promotion.jsonl"))?;
    let (event_tx, events) = promotion_event_collector();
    let head_before = git(&root, &["rev-parse", "HEAD"])?;

    let output = run_git_integration_promotion_with_events(
        GitIntegrationPromotionRunRequest {
            prepared,
            attempt_id: IntegrationPromotionAttemptId::new("attempt-workspace")?,
            preview,
            authority,
            verification_policy: promotion_policy(),
            mutation_recorder: MutationEventRecorder::new(mutation_store),
        },
        event_tx,
    )
    .await?;
    let events = events.await?;

    assert_eq!(
        output.record.status,
        IntegrationPromotionStatus::Promoted,
        "promotion failed: {:?}",
        output.record.reason
    );
    assert!(matches!(
        output.record.effect,
        Some(IntegrationPromotionEffect::WorkspaceApplied {
            promoted_revision: 1,
            ..
        })
    ));
    assert!(output.authoritative_snapshot_id.is_some());
    assert!(output.cleanup_error.is_none());
    assert_eq!(fs::read_to_string(root.join("a.txt"))?, "new-a\n");
    assert_eq!(fs::read_to_string(root.join("b.txt"))?, "new-b\n");
    assert_eq!(git(&root, &["rev-parse", "HEAD"])?, head_before);
    assert_promotion_event_sequence(&events, IntegrationPromotionStatus::Promoted);
    Ok(())
}

#[tokio::test]
async fn workspace_promotion_parent_drift_is_stale_with_zero_promotion_effect() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let change = change_set("promotion-stale", "a.txt", "old-a\n", "new-a\n")?;
    let artifacts = vec![artifact(
        change.clone(),
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
    )];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-workspace-stale")?,
        TaskId::new("task_workspace_stale")?,
        1,
        vec![proposal(
            &change,
            "step_workspace_stale",
            &base_snapshot_id,
            &base_commit,
        )?],
    )?;
    let lane_session = ready_lane_session(&root, &plan, &artifacts).await?;
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: "workspace-stale".to_owned(),
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: IntegrationPromotionPreparationTarget::WorkspaceApply {
            expected_snapshot_id: base_snapshot_id,
            expected_revision: 0,
        },
    })
    .await?;
    let preview = promotion_preview(&lane_session, &plan, &prepared)?;
    let authority = promotion_authority(&preview, "workspace-stale")?;
    fs::write(root.join("user-drift.txt"), "preserve me\n")?;
    let mutation_store = JsonlSessionStore::new(temp.path().join("promotion.jsonl"))?;

    let output = super::run_git_integration_promotion(GitIntegrationPromotionRunRequest {
        prepared,
        attempt_id: IntegrationPromotionAttemptId::new("attempt-workspace-stale")?,
        preview,
        authority,
        verification_policy: promotion_policy(),
        mutation_recorder: MutationEventRecorder::new(mutation_store),
    })
    .await?;

    assert_eq!(output.record.status, IntegrationPromotionStatus::Stale);
    assert!(output.record.effect.is_none());
    assert!(output.authoritative_snapshot_id.is_none());
    assert_eq!(fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    assert_eq!(
        fs::read_to_string(root.join("user-drift.txt"))?,
        "preserve me\n"
    );
    Ok(())
}

#[tokio::test]
async fn git_ref_promotion_advances_only_unchecked_out_target_by_cas() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    git(&root, &["branch", "review-target", &base_commit])?;
    let change = change_set("promotion-ref", "a.txt", "old-a\n", "new-a\n")?;
    let artifacts = vec![artifact(
        change.clone(),
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
    )];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-ref-promotion")?,
        TaskId::new("task_ref_promotion")?,
        1,
        vec![proposal(
            &change,
            "step_ref_promotion",
            &base_snapshot_id,
            &base_commit,
        )?],
    )?;
    let lane_session = ready_lane_session(&root, &plan, &artifacts).await?;
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: "ref-review".to_owned(),
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: IntegrationPromotionPreparationTarget::GitRefAdvance {
            target_ref: "refs/heads/review-target".to_owned(),
            expected_old_oid: base_commit.clone(),
        },
    })
    .await?;
    let candidate_oid = match prepared.target() {
        sigil_kernel::IntegrationPromotionTarget::GitRefAdvance { candidate_oid, .. } => {
            candidate_oid.clone()
        }
        _ => panic!("expected Git ref target"),
    };
    let preview = promotion_preview(&lane_session, &plan, &prepared)?;
    let authority = promotion_authority(&preview, "ref-review")?;
    let mutation_store = JsonlSessionStore::new(temp.path().join("promotion.jsonl"))?;
    let head_before = git(&root, &["rev-parse", "HEAD"])?;

    let output = super::run_git_integration_promotion(GitIntegrationPromotionRunRequest {
        prepared,
        attempt_id: IntegrationPromotionAttemptId::new("attempt-ref")?,
        preview,
        authority,
        verification_policy: promotion_policy(),
        mutation_recorder: MutationEventRecorder::new(mutation_store),
    })
    .await?;

    assert_eq!(output.record.status, IntegrationPromotionStatus::Promoted);
    assert_eq!(
        git(&root, &["rev-parse", "refs/heads/review-target"])?,
        candidate_oid
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"])?, head_before);
    assert_eq!(fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    assert_eq!(
        git(&root, &["show", "refs/heads/review-target:a.txt"])?,
        "new-a"
    );
    output
        .verification_target
        .expect("promoted Git ref must retain its authoritative checkout")
        .cleanup()
        .await?;
    Ok(())
}

#[tokio::test]
async fn git_ref_promotion_rejects_checked_out_target_without_ref_or_workspace_effect() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let checked_out_ref = git(&root, &["symbolic-ref", "HEAD"])?;
    let change = change_set("promotion-checked-out", "a.txt", "old-a\n", "new-a\n")?;
    let artifacts = vec![artifact(
        change.clone(),
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
    )];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-ref-checked-out")?,
        TaskId::new("task_ref_checked_out")?,
        1,
        vec![proposal(
            &change,
            "step_ref_checked_out",
            &base_snapshot_id,
            &base_commit,
        )?],
    )?;
    let lane_session = ready_lane_session(&root, &plan, &artifacts).await?;
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: "ref-checked-out".to_owned(),
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: IntegrationPromotionPreparationTarget::GitRefAdvance {
            target_ref: checked_out_ref.clone(),
            expected_old_oid: base_commit.clone(),
        },
    })
    .await?;
    let preview = promotion_preview(&lane_session, &plan, &prepared)?;
    let authority = promotion_authority(&preview, "ref-checked-out")?;
    let mutation_store = JsonlSessionStore::new(temp.path().join("promotion.jsonl"))?;

    let output = super::run_git_integration_promotion(GitIntegrationPromotionRunRequest {
        prepared,
        attempt_id: IntegrationPromotionAttemptId::new("attempt-ref-checked-out")?,
        preview,
        authority,
        verification_policy: promotion_policy(),
        mutation_recorder: MutationEventRecorder::new(mutation_store),
    })
    .await?;

    assert_eq!(output.record.status, IntegrationPromotionStatus::Conflict);
    assert!(output.record.effect.is_none());
    assert_eq!(git(&root, &["rev-parse", &checked_out_ref])?, base_commit);
    assert_eq!(fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    Ok(())
}

#[tokio::test]
async fn git_ref_promotion_stale_target_has_zero_ref_or_workspace_effect() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    git(&root, &["branch", "review-stale", &base_commit])?;
    let change = change_set("promotion-ref-stale", "a.txt", "old-a\n", "new-a\n")?;
    let artifacts = vec![artifact(
        change.clone(),
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
    )];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-ref-stale")?,
        TaskId::new("task_ref_stale")?,
        1,
        vec![proposal(
            &change,
            "step_ref_stale",
            &base_snapshot_id,
            &base_commit,
        )?],
    )?;
    let lane_session = ready_lane_session(&root, &plan, &artifacts).await?;
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: "ref-stale".to_owned(),
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: IntegrationPromotionPreparationTarget::GitRefAdvance {
            target_ref: "refs/heads/review-stale".to_owned(),
            expected_old_oid: base_commit.clone(),
        },
    })
    .await?;
    let preview = promotion_preview(&lane_session, &plan, &prepared)?;
    let authority = promotion_authority(&preview, "ref-stale")?;
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"])?;
    let drift_commit = git(
        &root,
        &["commit-tree", &tree, "-p", &base_commit, "-m", "ref drift"],
    )?;
    git(
        &root,
        &[
            "update-ref",
            "refs/heads/review-stale",
            &drift_commit,
            &base_commit,
        ],
    )?;
    let mutation_store = JsonlSessionStore::new(temp.path().join("promotion.jsonl"))?;

    let output = super::run_git_integration_promotion(GitIntegrationPromotionRunRequest {
        prepared,
        attempt_id: IntegrationPromotionAttemptId::new("attempt-ref-stale")?,
        preview,
        authority,
        verification_policy: promotion_policy(),
        mutation_recorder: MutationEventRecorder::new(mutation_store),
    })
    .await?;

    assert_eq!(output.record.status, IntegrationPromotionStatus::Stale);
    assert!(output.record.effect.is_none());
    assert_eq!(
        git(&root, &["rev-parse", "refs/heads/review-stale"])?,
        drift_commit
    );
    assert_eq!(fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    Ok(())
}

#[tokio::test]
async fn workspace_promotion_recovery_uses_complete_mutation_evidence_without_replay() -> Result<()>
{
    let mut fixture = interrupted_promotion_fixture(false, false).await?;
    assert_eq!(fs::read_to_string(fixture.root.join("a.txt"))?, "new-a\n");
    assert_eq!(
        IntegrationProjection::from_entries(fixture.session.entries())
            .plans
            .get(&fixture.plan_id)
            .expect("interrupted promotion plan")
            .promotions
            .len(),
        1
    );
    fixture
        .session
        .append_control(ControlEntry::VerificationPolicyChanged(
            VerificationPolicyChangedEntry::new(
                EvidenceScope::Task("task_workspace_recovery".to_owned()),
                VerificationPolicy::no_checks_required("superseding-recovery-policy"),
                "superseding-recovery-policy-event",
            )?,
        ))?;

    let report = reconcile_integration_promotions(&mut fixture.session, &fixture.root).await?;

    assert_eq!(report.inspected, 1);
    assert_eq!(report.reconciled, 1);
    assert_eq!(report.promoted, 1);
    assert_eq!(report.needs_review, 0);
    assert_eq!(report.recovered_promotions.len(), 1);
    assert_eq!(
        report.recovered_promotions[0].promoted_snapshot_id,
        fixture
            .promotion
            .as_ref()
            .expect("completed physical promotion")
            .authoritative_snapshot_id
            .as_deref()
            .expect("physical workspace snapshot")
    );
    assert_eq!(fs::read_to_string(fixture.root.join("a.txt"))?, "new-a\n");
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    let state = projection
        .plans
        .get(&fixture.plan_id)
        .expect("recovered workspace promotion");
    assert!(!state.inconsistent);
    assert_eq!(state.promotions.len(), 2);
    assert_eq!(
        state.promotions.last().map(|record| record.status),
        Some(IntegrationPromotionStatus::Promoted)
    );
    assert_eq!(
        reconcile_integration_promotions(&mut fixture.session, &fixture.root)
            .await?
            .inspected,
        0,
        "reconciliation must be idempotent"
    );
    Ok(())
}

#[tokio::test]
async fn git_ref_promotion_recovery_retains_exact_parent_check_target() -> Result<()> {
    let mut fixture = interrupted_promotion_fixture(true, false).await?;
    let candidate_oid = fixture
        .candidate_oid
        .as_deref()
        .expect("Git recovery candidate oid");
    assert_eq!(
        git(&fixture.root, &["rev-parse", "refs/heads/recovery-target"])?,
        candidate_oid
    );

    let report = reconcile_integration_promotions(&mut fixture.session, &fixture.root).await?;

    assert_eq!(report.inspected, 1);
    assert_eq!(report.promoted, 1);
    assert_eq!(report.needs_review, 0);
    assert_eq!(
        report.recovered_promotions[0]
            .retained_owned_workspace_id
            .as_deref(),
        Some(fixture.owned_workspace_id.as_str())
    );
    assert_eq!(
        report.recovered_promotions[0].promoted_snapshot_id,
        fixture
            .promotion
            .as_ref()
            .expect("completed physical promotion")
            .authoritative_snapshot_id
            .as_deref()
            .expect("physical Git candidate snapshot")
    );
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    let state = projection
        .plans
        .get(&fixture.plan_id)
        .expect("recovered Git promotion");
    assert!(!state.inconsistent);
    assert!(matches!(
        state.promotions.last().and_then(|record| record.effect.as_ref()),
        Some(IntegrationPromotionEffect::GitRefAdvanced { new_oid, .. })
            if new_oid == candidate_oid
    ));
    assert_eq!(
        reconcile_integration_promotions(&mut fixture.session, &fixture.root)
            .await?
            .inspected,
        0
    );
    fixture
        .promotion
        .as_mut()
        .expect("completed physical promotion")
        .verification_target
        .take()
        .expect("retained Git target")
        .cleanup()
        .await?;
    Ok(())
}

#[tokio::test]
async fn workspace_promotion_recovery_cancels_prepared_attempt_with_zero_effect() -> Result<()> {
    let mut fixture = interrupted_promotion_fixture(false, true).await?;
    assert!(fixture.promotion.is_none());
    assert_eq!(fs::read_to_string(fixture.root.join("a.txt"))?, "old-a\n");

    let report = reconcile_integration_promotions(&mut fixture.session, &fixture.root).await?;

    assert_eq!(report.inspected, 1);
    assert_eq!(report.reconciled, 1);
    assert_eq!(report.cancelled, 1);
    assert_eq!(report.promoted, 0);
    assert_eq!(report.needs_review, 0);
    assert_eq!(fs::read_to_string(fixture.root.join("a.txt"))?, "old-a\n");
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    let state = projection
        .plans
        .get(&fixture.plan_id)
        .expect("cancelled promotion projection");
    assert!(!state.inconsistent);
    assert_eq!(
        state.promotions.last().map(|record| record.status),
        Some(IntegrationPromotionStatus::Cancelled)
    );
    Ok(())
}

#[tokio::test]
async fn workspace_promotion_recovery_preserves_post_commit_drift_for_review() -> Result<()> {
    let mut fixture = interrupted_promotion_fixture(false, false).await?;
    fs::write(fixture.root.join("a.txt"), "user-drift\n")?;

    let report = reconcile_integration_promotions(&mut fixture.session, &fixture.root).await?;

    assert_eq!(report.inspected, 1);
    assert_eq!(report.reconciled, 0);
    assert_eq!(report.promoted, 0);
    assert_eq!(report.needs_review, 1);
    assert_eq!(
        fs::read_to_string(fixture.root.join("a.txt"))?,
        "user-drift\n"
    );
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    let state = projection
        .plans
        .get(&fixture.plan_id)
        .expect("drifted promotion projection");
    assert!(!state.inconsistent);
    assert_eq!(state.promotions.len(), 1);
    assert_eq!(
        state.promotions[0].status,
        IntegrationPromotionStatus::Prepared
    );
    Ok(())
}

#[tokio::test]
async fn rejected_authority_ack_starts_no_promotion_effect_and_cleans_candidate() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    let change = change_set("promotion-ack", "a.txt", "old-a\n", "new-a\n")?;
    let artifacts = vec![artifact(
        change.clone(),
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
    )];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-promotion-ack")?,
        TaskId::new("task_promotion_ack")?,
        1,
        vec![proposal(
            &change,
            "step_promotion_ack",
            &base_snapshot_id,
            &base_commit,
        )?],
    )?;
    let lane_session = ready_lane_session(&root, &plan, &artifacts).await?;
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: "promotion-ack".to_owned(),
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: IntegrationPromotionPreparationTarget::WorkspaceApply {
            expected_snapshot_id: base_snapshot_id,
            expected_revision: 0,
        },
    })
    .await?;
    let owned_workspace_id = prepared.owned_workspace_id().to_owned();
    let preview = promotion_preview(&lane_session, &plan, &prepared)?;
    let authority = promotion_authority(&preview, "promotion-ack")?;
    let mutation_store = JsonlSessionStore::new(temp.path().join("promotion.jsonl"))?;
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<IntegrationPromotionRuntimeEventRequest>();
    let rejector = tokio::spawn(async move {
        let request = event_rx.recv().await.expect("authority event");
        assert!(matches!(
            request.event(),
            IntegrationPromotionRuntimeEvent::AuthorityConsumed(_)
        ));
        request.acknowledge(Err("durable append rejected".to_owned()));
    });

    let error = run_git_integration_promotion_with_events(
        GitIntegrationPromotionRunRequest {
            prepared,
            attempt_id: IntegrationPromotionAttemptId::new("attempt-promotion-ack")?,
            preview,
            authority,
            verification_policy: promotion_policy(),
            mutation_recorder: MutationEventRecorder::new(mutation_store),
        },
        event_tx,
    )
    .await
    .expect_err("rejected durable authority ack must stop promotion");
    rejector.await?;

    assert!(format!("{error:#}").contains("durable event was rejected"));
    assert_eq!(fs::read_to_string(root.join("a.txt"))?, "old-a\n");
    assert!(
        !git(&root, &["worktree", "list", "--porcelain"])?.contains(&owned_workspace_id),
        "rejected promotion must clean its private candidate"
    );
    Ok(())
}

#[tokio::test]
async fn authoritative_parent_checks_pass_on_exact_promoted_workspace_snapshot() -> Result<()> {
    let mut fixture = promoted_workspace_fixture("test \"$(cat a.txt)\" = new-a").await?;
    let promoted_snapshot_id = fixture
        .promotion
        .authoritative_snapshot_id
        .take()
        .expect("workspace promotion snapshot");
    let target = fixture
        .promotion
        .verification_target
        .take()
        .expect("workspace promotion verification target");
    let mut handler = NoopEventHandler;

    let output = run_authoritative_parent_verification(
        &mut fixture.session,
        &mut handler,
        Arc::new(LocalExecutionBackend),
        ParentVerificationRunRequest {
            attempt_id: fixture.attempt_id.clone(),
            plan_id: fixture.plan_id.clone(),
            preview_digest: fixture.preview_digest,
            promoted_snapshot_id,
            policy_digest: fixture.policy.stable_hash()?,
            policy: fixture.policy,
            trusted_checks: vec![fixture.trusted_check],
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "parent-check-trust".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            target,
        },
    )
    .await?;

    assert_eq!(output.record.verdict, VerificationVerdict::Passed);
    assert_eq!(output.record.receipts.len(), 1);
    assert!(output.cleanup_error.is_none());
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    let state = projection
        .plans
        .get(&fixture.plan_id)
        .expect("integration plan projection");
    assert!(!state.inconsistent);
    assert_eq!(state.synthesis_ready_attempt(), Some(&fixture.attempt_id));
    Ok(())
}

#[tokio::test]
async fn authoritative_parent_checks_detect_snapshot_drift_before_execution() -> Result<()> {
    let mut fixture = promoted_workspace_fixture("exit 91").await?;
    fs::write(fixture.root.join("user-drift.txt"), "preserve me\n")?;
    let promoted_snapshot_id = fixture
        .promotion
        .authoritative_snapshot_id
        .take()
        .expect("workspace promotion snapshot");
    let target = fixture
        .promotion
        .verification_target
        .take()
        .expect("workspace promotion verification target");
    let mut handler = NoopEventHandler;

    let output = run_authoritative_parent_verification(
        &mut fixture.session,
        &mut handler,
        Arc::new(LocalExecutionBackend),
        ParentVerificationRunRequest {
            attempt_id: fixture.attempt_id.clone(),
            plan_id: fixture.plan_id.clone(),
            preview_digest: fixture.preview_digest,
            promoted_snapshot_id,
            policy_digest: fixture.policy.stable_hash()?,
            policy: fixture.policy,
            trusted_checks: vec![fixture.trusted_check],
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "parent-check-trust".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            target,
        },
    )
    .await?;

    assert_eq!(output.record.verdict, VerificationVerdict::Stale);
    assert!(output.record.receipts.is_empty());
    assert_eq!(
        fs::read_to_string(fixture.root.join("user-drift.txt"))?,
        "preserve me\n"
    );
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    let state = projection
        .plans
        .get(&fixture.plan_id)
        .expect("integration plan projection");
    assert!(!state.inconsistent);
    assert!(state.synthesis_ready_attempt().is_none());
    Ok(())
}

#[tokio::test]
async fn failed_authoritative_parent_check_never_opens_synthesis_gate() -> Result<()> {
    let mut fixture = promoted_workspace_fixture("exit 91").await?;
    let promoted_snapshot_id = fixture
        .promotion
        .authoritative_snapshot_id
        .take()
        .expect("workspace promotion snapshot");
    let target = fixture
        .promotion
        .verification_target
        .take()
        .expect("workspace promotion verification target");
    let mut handler = NoopEventHandler;

    let output = run_authoritative_parent_verification(
        &mut fixture.session,
        &mut handler,
        Arc::new(LocalExecutionBackend),
        ParentVerificationRunRequest {
            attempt_id: fixture.attempt_id.clone(),
            plan_id: fixture.plan_id.clone(),
            preview_digest: fixture.preview_digest,
            promoted_snapshot_id,
            policy_digest: fixture.policy.stable_hash()?,
            policy: fixture.policy,
            trusted_checks: vec![fixture.trusted_check],
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "parent-check-trust".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            target,
        },
    )
    .await?;

    assert_eq!(output.record.verdict, VerificationVerdict::Failed);
    assert_eq!(output.record.receipts.len(), 1);
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    assert!(
        projection
            .plans
            .get(&fixture.plan_id)
            .expect("integration plan projection")
            .synthesis_ready_attempt()
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn git_ref_parent_checks_use_retained_authoritative_checkout() -> Result<()> {
    let mut fixture = promoted_git_ref_fixture("test \"$(cat a.txt)\" = new-a").await?;
    let promoted_snapshot_id = fixture
        .promotion
        .authoritative_snapshot_id
        .take()
        .expect("Git ref promotion snapshot");
    let target = fixture
        .promotion
        .verification_target
        .take()
        .expect("Git ref promotion verification target");
    assert_ne!(target.workspace_root(), fixture.root);
    assert_eq!(fs::read_to_string(fixture.root.join("a.txt"))?, "old-a\n");
    let owned_workspace_id = target
        .workspace_root()
        .file_name()
        .expect("owned workspace name")
        .to_string_lossy()
        .into_owned();
    let mut handler = NoopEventHandler;

    let output = run_authoritative_parent_verification(
        &mut fixture.session,
        &mut handler,
        Arc::new(LocalExecutionBackend),
        ParentVerificationRunRequest {
            attempt_id: fixture.attempt_id.clone(),
            plan_id: fixture.plan_id.clone(),
            preview_digest: fixture.preview_digest,
            promoted_snapshot_id,
            policy_digest: fixture.policy.stable_hash()?,
            policy: fixture.policy,
            trusted_checks: vec![fixture.trusted_check],
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "parent-check-trust".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            target,
        },
    )
    .await?;

    assert_eq!(output.record.verdict, VerificationVerdict::Passed);
    assert!(output.cleanup_error.is_none());
    assert!(
        !git(&fixture.root, &["worktree", "list", "--porcelain"])?.contains(&owned_workspace_id)
    );
    assert_eq!(fs::read_to_string(fixture.root.join("a.txt"))?, "old-a\n");
    let projection = IntegrationProjection::from_entries(fixture.session.entries());
    assert_eq!(
        projection
            .plans
            .get(&fixture.plan_id)
            .and_then(|state| state.synthesis_ready_attempt()),
        Some(&fixture.attempt_id)
    );
    Ok(())
}

struct InterruptedPromotionFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    session: Session,
    promotion: Option<super::GitIntegrationPromotionOutput>,
    plan_id: IntegrationPlanId,
    candidate_oid: Option<String>,
    owned_workspace_id: String,
}

async fn interrupted_promotion_fixture(
    git_ref_target: bool,
    crash_after_prepared: bool,
) -> Result<InterruptedPromotionFixture> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    if git_ref_target {
        git(&root, &["branch", "recovery-target", &base_commit])?;
    }
    let change = change_set("promotion-recovery", "a.txt", "old-a\n", "new-a\n")?;
    let artifacts = vec![artifact(
        change.clone(),
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
    )];
    let plan = build_integration_plan(
        IntegrationPlanId::new(if git_ref_target {
            "plan-ref-recovery"
        } else {
            "plan-workspace-recovery"
        })?,
        TaskId::new(if git_ref_target {
            "task_ref_recovery"
        } else {
            "task_workspace_recovery"
        })?,
        1,
        vec![proposal(
            &change,
            "step_promotion_recovery",
            &base_snapshot_id,
            &base_commit,
        )?],
    )?;
    let lane_session = ready_lane_session(&root, &plan, &artifacts).await?;
    let store = JsonlSessionStore::new(temp.path().join("recovery-session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store.clone());
    for entry in lane_session.entries() {
        session.append(entry.clone())?;
    }
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: if git_ref_target {
            "ref-recovery-review".to_owned()
        } else {
            "workspace-recovery-review".to_owned()
        },
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: if git_ref_target {
            IntegrationPromotionPreparationTarget::GitRefAdvance {
                target_ref: "refs/heads/recovery-target".to_owned(),
                expected_old_oid: base_commit,
            }
        } else {
            IntegrationPromotionPreparationTarget::WorkspaceApply {
                expected_snapshot_id: base_snapshot_id,
                expected_revision: 0,
            }
        },
    })
    .await?;
    let candidate_oid = match prepared.target() {
        sigil_kernel::IntegrationPromotionTarget::GitRefAdvance { candidate_oid, .. } => {
            Some(candidate_oid.clone())
        }
        sigil_kernel::IntegrationPromotionTarget::WorkspaceApply { .. } => None,
    };
    let owned_workspace_id = prepared.owned_workspace_id().to_owned();
    let policy = promotion_policy();
    let preview = promotion_preview_with_policy(&session, &plan, &prepared, &policy)?;
    session.append_control(ControlEntry::VerificationPolicyChanged(
        VerificationPolicyChangedEntry::new(
            EvidenceScope::Task(plan.task_id.as_str().to_owned()),
            policy.clone(),
            "promotion-recovery-policy",
        )?,
    ))?;
    session.append_control(ControlEntry::TaskPromotionPreviewRecorded(
        TaskPromotionPreviewRecorded {
            preview: preview.clone(),
        },
    ))?;
    let authority = promotion_authority(
        &preview,
        if git_ref_target {
            "ref-recovery-review"
        } else {
            "workspace-recovery-review"
        },
    )?;
    let attempt_id = IntegrationPromotionAttemptId::new(if git_ref_target {
        "attempt-ref-recovery"
    } else {
        "attempt-workspace-recovery"
    })?;
    let (event_tx, collector) =
        interrupted_promotion_event_collector(store.clone(), crash_after_prepared);
    let promotion = run_git_integration_promotion_with_events(
        GitIntegrationPromotionRunRequest {
            prepared,
            attempt_id,
            preview,
            authority,
            verification_policy: policy,
            mutation_recorder: MutationEventRecorder::new(store.clone()),
        },
        event_tx,
    )
    .await;
    assert_eq!(collector.await??, if crash_after_prepared { 2 } else { 3 });
    let promotion = if crash_after_prepared {
        let error = promotion.expect_err("prepared crash must stop before the physical effect");
        assert!(
            format!("{error:#}")
                .contains("integration promotion durable event acknowledgement dropped")
        );
        None
    } else {
        Some(promotion?)
    };
    drop(session);
    let session = Session::load_from_store("mock", "model", store)?;
    Ok(InterruptedPromotionFixture {
        _temp: temp,
        root,
        session,
        promotion,
        plan_id: plan.plan_id,
        candidate_oid,
        owned_workspace_id,
    })
}

fn interrupted_promotion_event_collector(
    store: JsonlSessionStore,
    crash_after_prepared: bool,
) -> (
    tokio::sync::mpsc::UnboundedSender<IntegrationPromotionRuntimeEventRequest>,
    tokio::task::JoinHandle<Result<usize>>,
) {
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<IntegrationPromotionRuntimeEventRequest>();
    let collector = tokio::spawn(async move {
        let mut event_count = 0;
        while let Some(request) = event_rx.recv().await {
            event_count += 1;
            let is_prepared = matches!(
                request.event(),
                IntegrationPromotionRuntimeEvent::PromotionRecorded(entry)
                    if entry.status == IntegrationPromotionStatus::Prepared
            );
            let control = match request.event() {
                IntegrationPromotionRuntimeEvent::AuthorityConsumed(entry) => {
                    Some(ControlEntry::TaskPromotionAuthorityConsumed(entry.clone()))
                }
                IntegrationPromotionRuntimeEvent::PromotionRecorded(entry)
                    if entry.status == IntegrationPromotionStatus::Prepared =>
                {
                    Some(ControlEntry::IntegrationPromotionRecorded(entry.clone()))
                }
                IntegrationPromotionRuntimeEvent::PromotionRecorded(_) => None,
            };
            let acknowledgement = control.map_or(Ok(()), |control| {
                store
                    .append(&SessionLogEntry::Control(control))
                    .map_err(|error| format!("{error:#}"))
            });
            if crash_after_prepared && is_prepared {
                acknowledgement.map_err(anyhow::Error::msg)?;
                drop(request);
                break;
            }
            request.acknowledge(acknowledgement);
        }
        Ok(event_count)
    });
    (event_tx, collector)
}

struct PromotedWorkspaceFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    session: Session,
    promotion: super::GitIntegrationPromotionOutput,
    attempt_id: IntegrationPromotionAttemptId,
    plan_id: IntegrationPlanId,
    preview_digest: String,
    policy: VerificationPolicy,
    trusted_check: TrustedCheckSpec,
}

async fn promoted_workspace_fixture(check_script: &str) -> Result<PromotedWorkspaceFixture> {
    promoted_fixture(check_script, false).await
}

async fn promoted_git_ref_fixture(check_script: &str) -> Result<PromotedWorkspaceFixture> {
    promoted_fixture(check_script, true).await
}

async fn promoted_fixture(
    check_script: &str,
    git_ref_target: bool,
) -> Result<PromotedWorkspaceFixture> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let base_commit = git(&root, &["rev-parse", "HEAD"])?;
    if git_ref_target {
        git(&root, &["branch", "parent-check-target", &base_commit])?;
    }
    let change = change_set("promotion-parent-check", "a.txt", "old-a\n", "new-a\n")?;
    let artifacts = vec![artifact(
        change.clone(),
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-old-a\n+new-a\n",
    )];
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-parent-check")?,
        TaskId::new("task_parent_check")?,
        1,
        vec![proposal(
            &change,
            "step_parent_check",
            &base_snapshot_id,
            &base_commit,
        )?],
    )?;
    let mut session = ready_lane_session(&root, &plan, &artifacts).await?;
    let prepared = prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
        preparation_id: if git_ref_target {
            "parent-check-ref-review".to_owned()
        } else {
            "parent-check-review".to_owned()
        },
        parent_workspace_root: root.clone(),
        plan: plan.clone(),
        artifacts,
        frozen_base: None,
        target: if git_ref_target {
            IntegrationPromotionPreparationTarget::GitRefAdvance {
                target_ref: "refs/heads/parent-check-target".to_owned(),
                expected_old_oid: base_commit,
            }
        } else {
            IntegrationPromotionPreparationTarget::WorkspaceApply {
                expected_snapshot_id: base_snapshot_id,
                expected_revision: 0,
            }
        },
    })
    .await?;
    let trusted_check = trusted_parent_check(check_script)?;
    let policy = parent_verification_policy(&trusted_check);
    let preview = promotion_preview_with_policy(&session, &plan, &prepared, &policy)?;
    session.append_control(ControlEntry::TaskPromotionPreviewRecorded(
        TaskPromotionPreviewRecorded {
            preview: preview.clone(),
        },
    ))?;
    let authority = promotion_authority(
        &preview,
        if git_ref_target {
            "parent-check-ref-review"
        } else {
            "parent-check-review"
        },
    )?;
    let attempt_id = IntegrationPromotionAttemptId::new(if git_ref_target {
        "attempt-parent-check-ref"
    } else {
        "attempt-parent-check"
    })?;
    let mutation_store = JsonlSessionStore::new(temp.path().join("promotion.jsonl"))?;
    let (event_tx, events) = promotion_event_collector();
    let promotion = run_git_integration_promotion_with_events(
        GitIntegrationPromotionRunRequest {
            prepared,
            attempt_id: attempt_id.clone(),
            preview: preview.clone(),
            authority,
            verification_policy: policy.clone(),
            mutation_recorder: MutationEventRecorder::new(mutation_store),
        },
        event_tx,
    )
    .await?;
    append_promotion_events(&mut session, events.await?)?;
    Ok(PromotedWorkspaceFixture {
        _temp: temp,
        root,
        session,
        promotion,
        attempt_id,
        plan_id: plan.plan_id,
        preview_digest: preview.preview_digest,
        policy,
        trusted_check,
    })
}

fn trusted_parent_check(script: &str) -> Result<TrustedCheckSpec> {
    CandidateCheck {
        source: CheckDiscoverySource::RuntimeStructural,
        command: CheckCommand {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            cwd: None,
        },
        source_event_id: "parent-check-source".to_owned(),
        workspace_trust_snapshot_id: "parent-check-trust".to_owned(),
    }
    .promote(
        "parent-check",
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        ToolEffect::ReadOnly,
        CheckPromotion::GlobalPolicy {
            policy_event_id: "parent-check-policy".to_owned(),
        },
    )
}

fn parent_verification_policy(trusted_check: &TrustedCheckSpec) -> VerificationPolicy {
    VerificationPolicy {
        required_checks: vec![trusted_check.check_spec.clone()],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: Some(10_000),
        auto_run: VerificationAutoRunPolicy::Manual,
    }
}

fn append_promotion_events(
    session: &mut Session,
    events: Vec<IntegrationPromotionRuntimeEvent>,
) -> Result<()> {
    for event in events {
        let control = match event {
            IntegrationPromotionRuntimeEvent::AuthorityConsumed(entry) => {
                ControlEntry::TaskPromotionAuthorityConsumed(entry)
            }
            IntegrationPromotionRuntimeEvent::PromotionRecorded(entry) => {
                ControlEntry::IntegrationPromotionRecorded(entry)
            }
        };
        session.append_control(control)?;
    }
    Ok(())
}

async fn ready_lane_session(
    root: &Path,
    plan: &IntegrationPlan,
    artifacts: &[IntegrationArtifact],
) -> Result<Session> {
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<IntegrationLaneRuntimeEventRequest>();
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(request) = event_rx.recv().await {
            events.push(request.event().clone());
            request.acknowledge(Ok(()));
        }
        events
    });
    let output = run_git_integration_lanes_with_events(
        GitIntegrationRunRequest {
            parent_workspace_root: root.to_path_buf(),
            plan: plan.clone(),
            artifacts: artifacts.to_vec(),
            frozen_base: None,
            verification_backend: None,
        },
        Some(event_tx),
    )
    .await?;
    assert!(
        output
            .lanes
            .iter()
            .all(|lane| lane.status == IntegrationLaneStatus::Ready)
    );
    let events = collector.await?;
    let mut session = Session::new("mock", "model");
    session.append_control(ControlEntry::IntegrationPlanRecorded(
        IntegrationPlanRecorded { plan: plan.clone() },
    ))?;
    for event in events {
        let control = match event {
            IntegrationLaneRuntimeEvent::Prepared { entry, .. } => {
                ControlEntry::IntegrationLanePrepared(entry)
            }
            IntegrationLaneRuntimeEvent::MemberApplied(entry) => {
                ControlEntry::IntegrationLaneMemberApplied(entry)
            }
            IntegrationLaneRuntimeEvent::VerificationLinked(entry) => {
                ControlEntry::IntegrationLaneVerificationLinked(entry)
            }
            IntegrationLaneRuntimeEvent::Terminal(entry) => {
                ControlEntry::IntegrationLaneTerminal(entry)
            }
            IntegrationLaneRuntimeEvent::CleanupRecorded { entry, .. } => {
                ControlEntry::IntegrationLaneCleanupRecorded(entry)
            }
        };
        session.append_control(control)?;
    }
    Ok(session)
}

fn promotion_preview(
    session: &Session,
    plan: &IntegrationPlan,
    prepared: &super::PreparedGitIntegrationPromotion,
) -> Result<TaskPromotionPreview> {
    promotion_preview_with_policy(session, plan, prepared, &promotion_policy())
}

fn promotion_preview_with_policy(
    session: &Session,
    plan: &IntegrationPlan,
    prepared: &super::PreparedGitIntegrationPromotion,
    policy: &VerificationPolicy,
) -> Result<TaskPromotionPreview> {
    let projection = IntegrationProjection::from_entries(session.entries());
    let state = projection
        .plans
        .get(&plan.plan_id)
        .expect("integration plan projection");
    build_task_promotion_preview(
        state,
        TaskPromotionPreviewInput {
            aggregate_diff_artifact_ref: prepared.aggregate().artifact_ref.clone(),
            aggregate_diff_digest: prepared.aggregate_diff_digest(),
            target: prepared.target().clone(),
            verification_invalidation: vec![DEFAULT_TASK_VERIFICATION_SCOPE_HASH.to_owned()],
            intent_binding: None,
            policy_digest: policy.stable_hash()?,
            has_pending_approval: false,
            has_executable_intent_refs: false,
            created_at_unix_ms: test_unix_time_ms(),
        },
    )
}

fn promotion_policy() -> VerificationPolicy {
    VerificationPolicy::no_checks_required(DEFAULT_TASK_VERIFICATION_SCOPE_HASH)
}

fn promotion_authority(
    preview: &TaskPromotionPreview,
    review_id: &str,
) -> Result<TaskPromotionAuthority> {
    TaskPromotionAuthority::from_user_integration_review(
        preview,
        review_id,
        preview.created_at_unix_ms.saturating_add(60_000),
        format!("nonce-{review_id}"),
    )
}

fn promotion_event_collector() -> (
    tokio::sync::mpsc::UnboundedSender<IntegrationPromotionRuntimeEventRequest>,
    tokio::task::JoinHandle<Vec<IntegrationPromotionRuntimeEvent>>,
) {
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<IntegrationPromotionRuntimeEventRequest>();
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(request) = event_rx.recv().await {
            events.push(request.event().clone());
            request.acknowledge(Ok(()));
        }
        events
    });
    (event_tx, collector)
}

fn assert_promotion_event_sequence(
    events: &[IntegrationPromotionRuntimeEvent],
    terminal_status: IntegrationPromotionStatus,
) {
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events[0],
        IntegrationPromotionRuntimeEvent::AuthorityConsumed(_)
    ));
    assert!(matches!(
        &events[1],
        IntegrationPromotionRuntimeEvent::PromotionRecorded(record)
            if record.status == IntegrationPromotionStatus::Prepared
    ));
    assert!(matches!(
        &events[2],
        IntegrationPromotionRuntimeEvent::PromotionRecorded(record)
            if record.status == terminal_status
    ));
}

fn test_unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn proposal(
    change_set: &ChangeSet,
    step_id: &str,
    base_snapshot_id: &str,
    base_commit: &str,
) -> Result<IntegrationProposalSpec> {
    let facts = IntegrationProposalFacts::from_changeset(
        change_set,
        IntegrationBaseRepresentation::CleanCommit {
            base_commit: base_commit.to_owned(),
        },
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        format!("artifact-{}", change_set.id.as_str()),
        Vec::new(),
    )?;
    IntegrationProposalSpec::from_changeset(
        change_set,
        TaskStepId::new(step_id)?,
        base_snapshot_id.to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        facts,
    )
}

fn proposal_with_base(
    change_set: &ChangeSet,
    step_id: &str,
    base_snapshot_id: &str,
    base_representation: IntegrationBaseRepresentation,
) -> Result<IntegrationProposalSpec> {
    let facts = IntegrationProposalFacts::from_changeset(
        change_set,
        base_representation,
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        format!("artifact-{}", change_set.id.as_str()),
        Vec::new(),
    )?;
    IntegrationProposalSpec::from_changeset(
        change_set,
        TaskStepId::new(step_id)?,
        base_snapshot_id.to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        facts,
    )
}

fn artifact(change_set: ChangeSet, content: &str) -> IntegrationArtifact {
    IntegrationArtifact {
        change_set,
        content: content.to_owned(),
        content_sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
    }
}

fn change_set(id: &str, path: &str, before: &str, after: &str) -> Result<ChangeSet> {
    Ok(ChangeSet {
        id: ChangeSetId::new(id)?,
        title: id.to_owned(),
        summary: id.to_owned(),
        risk: ChangeSetRisk::Medium,
        files: vec![ChangeSetFile {
            path: path.to_owned(),
            previous_path: None,
            action: ChangeSetFileAction::Update,
            risk: ChangeSetRisk::Medium,
            before_hash: Some(format!("{:x}", Sha256::digest(before.as_bytes()))),
            after_hash: Some(format!("{:x}", Sha256::digest(after.as_bytes()))),
            diff_hash: None,
            additions: 1,
            deletions: 1,
            validations: Vec::new(),
        }],
        validations: Vec::new(),
    })
}

fn initialize_repository(root: &Path, files: &[(&str, &str)]) -> Result<()> {
    std::fs::create_dir_all(root)?;
    git(root, &["init", "--quiet"])?;
    git(root, &["config", "user.name", "Sigil Test"])?;
    git(root, &["config", "user.email", "sigil-test@localhost"])?;
    for (path, content) in files {
        std::fs::write(root.join(path), content)?;
    }
    git(root, &["add", "."])?;
    git(root, &["commit", "--quiet", "-m", "base"])?;
    Ok(())
}

fn workspace_snapshot_id(root: &Path) -> Result<String> {
    build_workspace_snapshot(
        root,
        stable_workspace_id(root)?,
        &VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        0,
    )?
    .workspace_snapshot_id
    .ok_or_else(|| anyhow::anyhow!("test workspace snapshot should be complete"))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn git_ref_exists(root: &Path, reference: &str) -> Result<bool> {
    Ok(Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show-ref", "--verify", "--quiet", reference])
        .status()?
        .success())
}
