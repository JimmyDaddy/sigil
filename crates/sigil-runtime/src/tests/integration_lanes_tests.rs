use std::{path::Path, process::Command};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, IntegrationEffect, IntegrationLaneCandidate,
    IntegrationLaneStatus, IntegrationPlanId, IntegrationProposalSpec, TaskId, TaskStepId,
    VerificationScope, build_integration_plan, build_workspace_snapshot, stable_workspace_id,
};

use super::{GitIntegrationRunRequest, IntegrationArtifact, run_git_integration_lanes};

#[tokio::test]
async fn disjoint_git_integration_lanes_overlap_and_preserve_parent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("a.txt", "old-a\n"), ("b.txt", "old-b\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let change_a = change_set("change-a", "a.txt")?;
    let change_b = change_set("change-b", "b.txt")?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-parallel")?,
        TaskId::new("task_parallel")?,
        1,
        vec![
            proposal(&change_a, "step_a", &base_snapshot_id)?,
            proposal(&change_b, "step_b", &base_snapshot_id)?,
        ],
    )?;
    assert_eq!(plan.lanes.len(), 2);

    let output = run_git_integration_lanes(GitIntegrationRunRequest {
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
    })
    .await?;

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
async fn conflicting_lane_fails_without_private_ref_or_parent_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    initialize_repository(&root, &[("shared.txt", "old\n")])?;
    let base_snapshot_id = workspace_snapshot_id(&root)?;
    let first = change_set("change-first", "shared.txt")?;
    let second = change_set("change-second", "shared.txt")?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-conflict")?,
        TaskId::new("task_conflict")?,
        1,
        vec![
            proposal(&first, "step_first", &base_snapshot_id)?,
            proposal(&second, "step_second", &base_snapshot_id)?,
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
    })
    .await?;

    assert_eq!(output.lanes.len(), 1);
    assert_eq!(output.lanes[0].status, IntegrationLaneStatus::Conflict);
    assert!(output.lanes[0].candidate.is_none());
    assert_eq!(std::fs::read_to_string(root.join("shared.txt"))?, "old\n");
    assert!(!git_ref_exists(
        &root,
        "refs/sigil/integration/plan-conflict/lane-1"
    )?);
    Ok(())
}

fn proposal(
    change_set: &ChangeSet,
    step_id: &str,
    base_snapshot_id: &str,
) -> Result<IntegrationProposalSpec> {
    IntegrationProposalSpec::from_changeset(
        change_set,
        TaskStepId::new(step_id)?,
        base_snapshot_id.to_owned(),
        Vec::new(),
        Vec::new(),
        IntegrationEffect::Files,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    )
}

fn artifact(change_set: ChangeSet, content: &str) -> IntegrationArtifact {
    IntegrationArtifact {
        change_set,
        content: content.to_owned(),
        content_sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
    }
}

fn change_set(id: &str, path: &str) -> Result<ChangeSet> {
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
            before_hash: None,
            after_hash: None,
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
