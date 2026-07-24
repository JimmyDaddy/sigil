//! Physical Git integration lanes for isolated task changesets.
//!
//! Lane planning and durable state live in `sigil-kernel`. This module materializes one private
//! worktree per conflict component, applies lane proposals in deterministic order, performs
//! bounded structural verification, and advances only runtime-owned private refs.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use futures::future::join_all;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ChangeSet, IntegrationBaseRepresentation, IntegrationLaneCandidate, IntegrationLaneId,
    IntegrationLaneStatus, IntegrationPlan, IntegrationPlanId, WorkspaceSnapshotId,
};

use crate::isolated_workspace::{
    GitWorktreeMaterializationRequest, MaterializedGitWorktree, materialize_git_worktree,
    run_git_bytes, run_git_bytes_with_stdin,
};

const ZERO_GIT_OBJECT_ID: &str = "0000000000000000000000000000000000000000";
const MAX_INTEGRATION_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;
const MAX_INTEGRATION_GIT_OUTPUT_BYTES: usize = 64 * 1024;

/// One content-bound proposal supplied to a physical lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationArtifact {
    pub change_set: ChangeSet,
    pub content: String,
    pub content_sha256: String,
}

/// Physical execution request for a deterministic integration plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIntegrationRunRequest {
    pub parent_workspace_root: PathBuf,
    pub plan: IntegrationPlan,
    pub artifacts: Vec<IntegrationArtifact>,
}

/// Terminal physical result for one lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIntegrationLaneResult {
    pub plan_id: IntegrationPlanId,
    pub lane_id: IntegrationLaneId,
    pub status: IntegrationLaneStatus,
    pub candidate: Option<IntegrationLaneCandidate>,
    pub verification_check_ids: Vec<String>,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub reason: Option<String>,
    pub cleanup_error: Option<String>,
}

/// Stable request-order results for all physical lanes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIntegrationRunOutput {
    pub lanes: Vec<GitIntegrationLaneResult>,
}

/// Materializes and executes independent integration lanes concurrently.
///
/// Worktree creation is intentionally completed before lane futures are launched so every child is
/// bound to the exact same parent snapshot and Git worktree administration stays serialized. Patch
/// application, structural verification, commit creation, and private-ref CAS then overlap across
/// lanes. Results return in deterministic lane order.
///
/// # Errors
///
/// Returns an error before any lane execution for malformed/missing artifacts, an inconsistent
/// plan, or worktree materialization failure. Per-lane apply/verification/ref failures are returned
/// as terminal lane results and never advance the parent ref/workspace.
pub async fn run_git_integration_lanes(
    request: GitIntegrationRunRequest,
) -> Result<GitIntegrationRunOutput> {
    validate_integration_request(&request)?;
    let IntegrationBaseRepresentation::CleanCommit {
        base_commit: planned_base_commit,
    } = &request.plan.base_representation
    else {
        bail!("managed-ref integration requires a clean commit base");
    };
    let planned_base_commit = planned_base_commit.clone();
    let artifacts = request
        .artifacts
        .into_iter()
        .map(|artifact| (artifact.change_set.id.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut materialized: Vec<(
        IntegrationPlanId,
        IntegrationLaneId,
        MaterializedGitWorktree,
        Vec<IntegrationArtifact>,
    )> = Vec::with_capacity(request.plan.lanes.len());
    for lane in &request.plan.lanes {
        let workspace_id = git_integration_workspace_id(&request.plan.plan_id, &lane.lane_id);
        let workspace = match materialize_git_worktree(GitWorktreeMaterializationRequest {
            parent_workspace_root: request.parent_workspace_root.clone(),
            isolated_workspace_id: workspace_id,
            base_snapshot_id: request.plan.base_snapshot_id.clone(),
        })
        .await
        .with_context(|| {
            format!(
                "failed to materialize integration lane {}",
                lane.lane_id.as_str()
            )
        }) {
            Ok(workspace) => workspace,
            Err(error) => {
                for (_, _, workspace, _) in materialized {
                    let _ = workspace.cleanup().await;
                }
                return Err(error);
            }
        };
        if workspace.base_commit() != planned_base_commit {
            let observed_base_commit = workspace.base_commit().to_owned();
            let _ = workspace.cleanup().await;
            for (_, _, workspace, _) in materialized {
                let _ = workspace.cleanup().await;
            }
            bail!(
                "integration base commit drifted: expected {planned_base_commit}, observed {observed_base_commit}"
            );
        }
        let lane_artifacts = lane
            .proposals
            .iter()
            .map(|id| {
                artifacts
                    .get(id)
                    .cloned()
                    .ok_or_else(|| anyhow!("missing integration artifact {}", id.as_str()))
            })
            .collect::<Result<Vec<_>>>()?;
        materialized.push((
            request.plan.plan_id.clone(),
            lane.lane_id.clone(),
            workspace,
            lane_artifacts,
        ));
    }

    let lane_futures =
        materialized
            .into_iter()
            .map(|(plan_id, lane_id, workspace, lane_artifacts)| {
                execute_git_integration_lane(plan_id, lane_id, workspace, lane_artifacts)
            });
    let lanes = join_all(lane_futures).await;
    Ok(GitIntegrationRunOutput { lanes })
}

async fn execute_git_integration_lane(
    plan_id: IntegrationPlanId,
    lane_id: IntegrationLaneId,
    workspace: MaterializedGitWorktree,
    artifacts: Vec<IntegrationArtifact>,
) -> GitIntegrationLaneResult {
    let started_at_unix_ms = unix_time_ms();
    let integration_ref = integration_ref(&plan_id, &lane_id);
    let base_commit = workspace.base_commit().to_owned();
    let execution = execute_lane_changes(&workspace, &integration_ref, &artifacts).await;
    let cleanup = workspace.cleanup().await;
    let cleanup_error = cleanup.err().map(|error| format!("{error:#}"));
    let finished_at_unix_ms = unix_time_ms();
    match execution {
        Ok((commit, snapshot_id, verification_check_ids)) => GitIntegrationLaneResult {
            plan_id,
            lane_id,
            status: if cleanup_error.is_some() {
                IntegrationLaneStatus::Failed
            } else {
                IntegrationLaneStatus::Ready
            },
            candidate: Some(IntegrationLaneCandidate::ManagedRef {
                private_ref: integration_ref,
                base_commit,
                candidate_commit: commit,
                workspace_snapshot_id: snapshot_id,
            }),
            verification_check_ids,
            started_at_unix_ms,
            finished_at_unix_ms,
            reason: cleanup_error
                .as_ref()
                .map(|_| "integration lane cleanup failed after private ref creation".to_owned()),
            cleanup_error,
        },
        Err(error) => {
            let reason = format!("{error:#}");
            GitIntegrationLaneResult {
                plan_id,
                lane_id,
                status: classify_lane_failure(&reason),
                candidate: None,
                verification_check_ids: Vec::new(),
                started_at_unix_ms,
                finished_at_unix_ms,
                reason: Some(reason),
                cleanup_error,
            }
        }
    }
}

async fn execute_lane_changes(
    workspace: &MaterializedGitWorktree,
    integration_ref: &str,
    artifacts: &[IntegrationArtifact],
) -> Result<(String, WorkspaceSnapshotId, Vec<String>)> {
    let root = workspace.workspace_root();
    for artifact in artifacts {
        run_git_bytes_with_stdin(
            root,
            [
                OsString::from("apply"),
                OsString::from("--check"),
                OsString::from("--whitespace=error-all"),
                OsString::from("-"),
            ],
            artifact.content.as_bytes(),
            MAX_INTEGRATION_GIT_OUTPUT_BYTES,
        )
        .await
        .with_context(|| {
            format!(
                "integration conflict while checking changeset {}",
                artifact.change_set.id.as_str()
            )
        })?;
        run_git_bytes_with_stdin(
            root,
            [
                OsString::from("apply"),
                OsString::from("--index"),
                OsString::from("--whitespace=error-all"),
                OsString::from("-"),
            ],
            artifact.content.as_bytes(),
            MAX_INTEGRATION_GIT_OUTPUT_BYTES,
        )
        .await
        .with_context(|| {
            format!(
                "integration conflict while applying changeset {}",
                artifact.change_set.id.as_str()
            )
        })?;
    }

    run_git_bytes(
        root,
        [
            OsString::from("diff"),
            OsString::from("--cached"),
            OsString::from("--check"),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    .context("scoped integration verification git diff --check failed")?;
    run_git_bytes(
        root,
        [
            OsString::from("-c"),
            OsString::from("user.name=Sigil Integration"),
            OsString::from("-c"),
            OsString::from("user.email=sigil-integration@localhost"),
            OsString::from("commit"),
            OsString::from("--quiet"),
            OsString::from("-m"),
            OsString::from(format!(
                "sigil integration {}",
                artifacts
                    .iter()
                    .map(|artifact| artifact.change_set.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            )),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    .context("failed to commit verified integration lane")?;
    let commit = git_text(
        root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("HEAD^{commit}"),
        ],
    )
    .await?;
    advance_private_ref(root, integration_ref, &commit).await?;
    let snapshot_id = workspace
        .current_snapshot_id()
        .await
        .context("failed to capture verified integration lane snapshot")?;
    Ok((
        commit,
        snapshot_id,
        vec!["git-apply-check".to_owned(), "git-diff-check".to_owned()],
    ))
}

async fn advance_private_ref(root: &std::path::Path, reference: &str, commit: &str) -> Result<()> {
    let observed = git_optional_text(
        root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(reference),
        ],
    )
    .await?;
    match observed {
        Some(observed) if observed == commit => Ok(()),
        Some(observed) => bail!(
            "private integration ref {reference} is stale: expected absent or {commit}, observed {observed}"
        ),
        None => {
            run_git_bytes(
                root,
                [
                    OsString::from("update-ref"),
                    OsString::from(reference),
                    OsString::from(commit),
                    OsString::from(ZERO_GIT_OBJECT_ID),
                ],
                MAX_INTEGRATION_GIT_OUTPUT_BYTES,
            )
            .await
            .with_context(|| format!("failed to CAS private integration ref {reference}"))?;
            Ok(())
        }
    }
}

fn validate_integration_request(request: &GitIntegrationRunRequest) -> Result<()> {
    if request.plan.lanes.is_empty() {
        bail!("physical integration requires at least one lane");
    }
    for proposal in &request.plan.proposals {
        proposal.validate()?;
    }
    if request.plan.requires_manual_review()
        || !matches!(
            request.plan.base_representation,
            IntegrationBaseRepresentation::CleanCommit { .. }
        )
    {
        bail!("managed-ref integration requires complete facts from one clean commit base");
    }
    let expected = request
        .plan
        .proposals
        .iter()
        .map(|proposal| proposal.change_set_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut observed = std::collections::BTreeSet::new();
    for artifact in &request.artifacts {
        if artifact.content.is_empty() || artifact.content.len() > MAX_INTEGRATION_ARTIFACT_BYTES {
            bail!(
                "integration artifact {} is empty or exceeds the {} byte limit",
                artifact.change_set.id.as_str(),
                MAX_INTEGRATION_ARTIFACT_BYTES
            );
        }
        let hash = format!("{:x}", Sha256::digest(artifact.content.as_bytes()));
        if hash != artifact.content_sha256 {
            bail!(
                "integration artifact {} hash mismatch",
                artifact.change_set.id.as_str()
            );
        }
        if !observed.insert(artifact.change_set.id.clone()) {
            bail!("physical integration contains a duplicate artifact");
        }
    }
    if expected != observed {
        bail!("physical integration artifacts do not exactly match the planned proposals");
    }
    Ok(())
}

fn classify_lane_failure(reason: &str) -> IntegrationLaneStatus {
    if reason.contains("parent workspace snapshot drifted") {
        IntegrationLaneStatus::Stale
    } else if reason.contains("integration conflict") || reason.contains("patch failed") {
        IntegrationLaneStatus::Conflict
    } else {
        IntegrationLaneStatus::Failed
    }
}

#[must_use]
pub fn git_integration_workspace_id(
    plan_id: &IntegrationPlanId,
    lane_id: &IntegrationLaneId,
) -> String {
    let digest = Sha256::digest(format!("{}:{}", plan_id.as_str(), lane_id.as_str()).as_bytes());
    format!("integration-{digest:x}")
}

fn integration_ref(plan_id: &IntegrationPlanId, lane_id: &IntegrationLaneId) -> String {
    format!(
        "refs/sigil/integration/{}/{}",
        plan_id.as_str(),
        lane_id.as_str()
    )
}

async fn git_text(
    root: &std::path::Path,
    args: impl IntoIterator<Item = OsString>,
) -> Result<String> {
    let output = run_git_bytes(root, args, MAX_INTEGRATION_GIT_OUTPUT_BYTES).await?;
    let text = String::from_utf8(output).context("Git output is not valid UTF-8")?;
    let text = text.trim();
    if text.is_empty() {
        bail!("Git command returned empty output");
    }
    Ok(text.to_owned())
}

async fn git_optional_text(
    root: &std::path::Path,
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<String>> {
    match run_git_bytes(root, args, MAX_INTEGRATION_GIT_OUTPUT_BYTES).await {
        Ok(output) => {
            let text = String::from_utf8(output).context("Git output is not valid UTF-8")?;
            Ok(Some(text.trim().to_owned()))
        }
        Err(error) if format!("{error:#}").contains("status exit status: 1") => Ok(None),
        Err(error) => Err(error),
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
#[path = "tests/integration_lanes_tests.rs"]
mod tests;
