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
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionFuture, ExecutionNetworkReceipt, ExecutionRequest,
    IntegrationBaseRepresentation, IntegrationContentClass, IntegrationEffect,
    IntegrationLaneCandidate, IntegrationLaneCleanupStatus, IntegrationLaneStatus,
    IntegrationPlanId, IntegrationProposalFacts, IntegrationProposalSpec, JsonlSessionStore,
    MutationEventRecorder, TaskId, TaskStepId, VerificationScope, build_integration_plan,
    build_workspace_snapshot, stable_workspace_id,
};
use sigil_tools_builtin::LocalExecutionBackend;

use super::{
    GitIntegrationRunRequest, IntegrationArtifact, IntegrationLaneRuntimeEvent,
    IntegrationLaneRuntimeEventRequest, run_git_integration_lanes,
    run_git_integration_lanes_with_events,
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
