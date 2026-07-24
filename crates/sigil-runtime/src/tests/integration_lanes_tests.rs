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
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk, ControlEntry,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionFuture, ExecutionNetworkReceipt, ExecutionRequest,
    IntegrationBaseRepresentation, IntegrationContentClass, IntegrationEffect,
    IntegrationLaneCandidate, IntegrationLaneCleanupStatus, IntegrationLaneStatus, IntegrationPlan,
    IntegrationPlanId, IntegrationPlanRecorded, IntegrationProjection,
    IntegrationPromotionAttemptId, IntegrationPromotionEffect, IntegrationPromotionStatus,
    IntegrationProposalFacts, IntegrationProposalSpec, JsonlSessionStore, MutationEventRecorder,
    Session, TaskId, TaskPromotionAuthority, TaskPromotionPreview, TaskPromotionPreviewInput,
    TaskStepId, VerificationScope, build_integration_plan, build_task_promotion_preview,
    build_workspace_snapshot, stable_workspace_id,
};
use sigil_tools_builtin::LocalExecutionBackend;

use super::{
    GitIntegrationPromotionPreparationRequest, GitIntegrationPromotionRunRequest,
    GitIntegrationRunRequest, IntegrationArtifact, IntegrationLaneRuntimeEvent,
    IntegrationLaneRuntimeEventRequest, IntegrationPromotionPreparationTarget,
    IntegrationPromotionRuntimeEvent, IntegrationPromotionRuntimeEventRequest,
    prepare_git_integration_promotion, run_git_integration_lanes,
    run_git_integration_lanes_with_events, run_git_integration_promotion_with_events,
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
            policy_digest: format!("sha256:{}", "b".repeat(64)),
            has_pending_approval: false,
            has_executable_intent_refs: false,
            created_at_unix_ms: test_unix_time_ms(),
        },
    )
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
