//! Physical Git integration lanes for isolated task changesets.
//!
//! Lane planning and durable state live in `sigil-kernel`. This module materializes one private
//! worktree per conflict component, applies lane proposals in deterministic order, performs
//! bounded structural verification, and advances only runtime-owned private refs.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use futures::future::join_all;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ChangeSet, ChangeSetFileAction, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CompletionCriteria, EvidenceScope, ExecutionBackend, IntegrationBaseRepresentation,
    IntegrationLaneCandidate, IntegrationLaneCleanupRecorded, IntegrationLaneCleanupStatus,
    IntegrationLaneId, IntegrationLaneMemberApplied, IntegrationLaneMemberEffect,
    IntegrationLanePrepared, IntegrationLaneStatus, IntegrationLaneTarget, IntegrationLaneTerminal,
    IntegrationLaneVerificationLinked, IntegrationPlan, IntegrationPlanId,
    IsolatedWorkspaceCleanupStatus, ReceiptStatus, SandboxProfileRequirement, Session, ToolEffect,
    TrustedCheckSpec, VerificationAutoRunPolicy, VerificationCheckRunRequest, VerificationPolicy,
    VerificationReceipt, VerificationScope, WorkspaceSnapshotId, WorkspaceTrust,
    run_verification_check, stable_event_uuid,
};
use sigil_tools_builtin::LocalExecutionBackend;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::isolated_workspace::{
    FrozenGitWorktreeBase, GitWorktreeMaterializationRequest, MaterializedGitWorktree,
    materialize_git_worktree, materialize_git_worktree_from_frozen_base, run_git_bytes,
    run_git_bytes_with_stdin,
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
#[derive(Clone)]
pub struct GitIntegrationRunRequest {
    pub parent_workspace_root: PathBuf,
    pub plan: IntegrationPlan,
    pub artifacts: Vec<IntegrationArtifact>,
    pub frozen_base: Option<FrozenGitWorktreeBase>,
    pub verification_backend: Option<Arc<dyn ExecutionBackend>>,
}

/// Recovery-critical runtime event emitted at the physical lane boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationLaneRuntimeEvent {
    Prepared {
        entry: IntegrationLanePrepared,
        materialized_snapshot_id: WorkspaceSnapshotId,
    },
    MemberApplied(IntegrationLaneMemberApplied),
    VerificationLinked(IntegrationLaneVerificationLinked),
    Terminal(IntegrationLaneTerminal),
    CleanupRecorded {
        entry: IntegrationLaneCleanupRecorded,
        workspace_status: IsolatedWorkspaceCleanupStatus,
    },
}

/// One event plus an acknowledgement required before the physical lane may continue.
#[derive(Debug)]
pub struct IntegrationLaneRuntimeEventRequest {
    event: IntegrationLaneRuntimeEvent,
    acknowledgement: oneshot::Sender<std::result::Result<(), String>>,
}

impl IntegrationLaneRuntimeEventRequest {
    #[must_use]
    pub fn event(&self) -> &IntegrationLaneRuntimeEvent {
        &self.event
    }

    pub fn acknowledge(self, result: std::result::Result<(), String>) {
        let _ = self.acknowledgement.send(result);
    }
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

type MaterializedIntegrationLane = (
    IntegrationPlanId,
    IntegrationLaneId,
    MaterializedGitWorktree,
    Vec<IntegrationArtifact>,
    IntegrationLaneTarget,
    Vec<String>,
);

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
    run_git_integration_lanes_with_events(request, None).await
}

/// Runs integration lanes while emitting recovery-critical facts before subsequent effects.
///
/// # Errors
///
/// Returns the same validation/materialization failures as [`run_git_integration_lanes`], plus an
/// error when the durable event receiver closes before a required receipt is accepted.
pub async fn run_git_integration_lanes_with_events(
    request: GitIntegrationRunRequest,
    event_sender: Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
) -> Result<GitIntegrationRunOutput> {
    validate_integration_request(&request)?;
    validate_physical_base(&request)?;
    let verification_backend = request
        .verification_backend
        .clone()
        .unwrap_or_else(|| Arc::new(LocalExecutionBackend));
    let artifacts = request
        .artifacts
        .iter()
        .cloned()
        .map(|artifact| (artifact.change_set.id.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut materialized =
        Vec::<MaterializedIntegrationLane>::with_capacity(request.plan.lanes.len());
    for lane in &request.plan.lanes {
        let workspace_id = git_integration_workspace_id(&request.plan.plan_id, &lane.lane_id);
        let workspace = match materialize_lane_workspace(&request, workspace_id.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to materialize integration lane {}",
                    lane.lane_id.as_str()
                )
            }) {
            Ok(workspace) => workspace,
            Err(error) => {
                for (plan_id, lane_id, workspace, _, _, _) in materialized {
                    cleanup_lane_workspace(
                        &event_sender,
                        plan_id,
                        lane_id,
                        workspace,
                        IntegrationLaneCleanupStatus::Removed,
                    )
                    .await;
                }
                return Err(error);
            }
        };
        if let Err(error) = validate_materialized_base(&request.plan, &workspace) {
            cleanup_failed_preparation(
                &event_sender,
                request.plan.plan_id.clone(),
                lane.lane_id.clone(),
                workspace,
                materialized,
            )
            .await;
            return Err(error);
        }
        let lane_artifacts = match lane
            .proposals
            .iter()
            .map(|id| {
                artifacts
                    .get(id)
                    .cloned()
                    .ok_or_else(|| anyhow!("missing integration artifact {}", id.as_str()))
            })
            .collect::<Result<Vec<_>>>()
        {
            Ok(artifacts) => artifacts,
            Err(error) => {
                cleanup_failed_preparation(
                    &event_sender,
                    request.plan.plan_id.clone(),
                    lane.lane_id.clone(),
                    workspace,
                    materialized,
                )
                .await;
                return Err(error);
            }
        };
        let target = match lane_target(&request.plan, &lane.lane_id, &workspace) {
            Ok(target) => target,
            Err(error) => {
                cleanup_failed_preparation(
                    &event_sender,
                    request.plan.plan_id.clone(),
                    lane.lane_id.clone(),
                    workspace,
                    materialized,
                )
                .await;
                return Err(error);
            }
        };
        let prepared = emit_event(
            &event_sender,
            IntegrationLaneRuntimeEvent::Prepared {
                entry: IntegrationLanePrepared {
                    plan_id: request.plan.plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    target: target.clone(),
                    owned_workspace_id: workspace.isolated_workspace_id().to_owned(),
                    ordered_members: lane.proposals.clone(),
                    prepared_at_unix_ms: unix_time_ms(),
                },
                materialized_snapshot_id: workspace.child_snapshot_id().to_owned(),
            },
        )
        .await;
        if let Err(error) = prepared {
            cleanup_failed_preparation(
                &event_sender,
                request.plan.plan_id.clone(),
                lane.lane_id.clone(),
                workspace,
                materialized,
            )
            .await;
            return Err(error);
        }
        materialized.push((
            request.plan.plan_id.clone(),
            lane.lane_id.clone(),
            workspace,
            lane_artifacts,
            target,
            lane.verification_scope_hashes.clone(),
        ));
    }

    let lane_futures = materialized.into_iter().map(
        |(plan_id, lane_id, workspace, lane_artifacts, target, verification_scope_hashes)| {
            execute_git_integration_lane(
                plan_id,
                lane_id,
                workspace,
                lane_artifacts,
                target,
                verification_scope_hashes,
                verification_backend.clone(),
                event_sender.clone(),
            )
        },
    );
    let lanes = join_all(lane_futures).await;
    Ok(GitIntegrationRunOutput { lanes })
}

async fn execute_git_integration_lane(
    plan_id: IntegrationPlanId,
    lane_id: IntegrationLaneId,
    workspace: MaterializedGitWorktree,
    artifacts: Vec<IntegrationArtifact>,
    target: IntegrationLaneTarget,
    verification_scope_hashes: Vec<String>,
    verification_backend: Arc<dyn ExecutionBackend>,
    event_sender: Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
) -> GitIntegrationLaneResult {
    let started_at_unix_ms = unix_time_ms();
    let workspace_id = workspace.isolated_workspace_id().to_owned();
    let apply_result = match &target {
        IntegrationLaneTarget::ManagedRef { .. } => {
            execute_managed_ref_lane(
                &plan_id,
                &lane_id,
                &workspace,
                &artifacts,
                &target,
                &event_sender,
            )
            .await
        }
        IntegrationLaneTarget::SnapshotWorkspace { .. } => {
            execute_snapshot_workspace_lane(
                &plan_id,
                &lane_id,
                &workspace,
                &artifacts,
                &target,
                &event_sender,
            )
            .await
        }
    };
    let mut cleanup_error = None;
    let mut retained = false;
    let (candidate, verification_check_ids, error) = match apply_result {
        Ok(candidate) => {
            match verify_lane_candidate(
                &plan_id,
                &lane_id,
                &workspace,
                &artifacts,
                &target,
                &candidate,
                &verification_scope_hashes,
                verification_backend.as_ref(),
            )
            .await
            {
                Ok(verification_receipts) => {
                    let verification_check_ids = verification_receipts
                        .iter()
                        .map(|receipt| receipt.check_spec_id.clone())
                        .collect::<Vec<_>>();
                    if let Err(error) = emit_event(
                        &event_sender,
                        IntegrationLaneRuntimeEvent::VerificationLinked(
                            IntegrationLaneVerificationLinked {
                                plan_id: plan_id.clone(),
                                lane_id: lane_id.clone(),
                                candidate: candidate.clone(),
                                verification_check_ids: verification_check_ids.clone(),
                                verification_scope_hashes,
                                verification_receipts,
                                linked_at_unix_ms: unix_time_ms(),
                            },
                        ),
                    )
                    .await
                    {
                        (None, Vec::new(), Some(error))
                    } else {
                        (Some(candidate), verification_check_ids, None)
                    }
                }
                Err(error) => (None, Vec::new(), Some(error)),
            }
        }
        Err(error) => (None, Vec::new(), Some(error)),
    };

    let snapshot_success =
        error.is_none() && matches!(target, IntegrationLaneTarget::SnapshotWorkspace { .. });
    if snapshot_success {
        retained = true;
        let _ = emit_cleanup_event(
            &event_sender,
            &plan_id,
            &lane_id,
            &workspace_id,
            IntegrationLaneCleanupStatus::Retained,
            IsolatedWorkspaceCleanupStatus::Retained,
        )
        .await;
    } else {
        let intended_status = IntegrationLaneCleanupStatus::Removed;
        match workspace.cleanup().await {
            Ok(receipt) => {
                if let Err(error) = emit_cleanup_event(
                    &event_sender,
                    &plan_id,
                    &lane_id,
                    &workspace_id,
                    cleanup_status_from_workspace(receipt.status),
                    receipt.status,
                )
                .await
                {
                    cleanup_error = Some(format!("{error:#}"));
                }
            }
            Err(cleanup) => {
                cleanup_error = Some(format!("{cleanup:#}"));
                let _ = emit_cleanup_event(
                    &event_sender,
                    &plan_id,
                    &lane_id,
                    &workspace_id,
                    IntegrationLaneCleanupStatus::Failed,
                    IsolatedWorkspaceCleanupStatus::Failed,
                )
                .await;
                let _ = intended_status;
            }
        }
    }

    let reason = match (error, cleanup_error.as_ref()) {
        (Some(error), Some(cleanup)) => Some(format!("{error:#}; cleanup failed: {cleanup}")),
        (Some(error), None) => Some(format!("{error:#}")),
        (None, Some(cleanup)) => Some(format!("integration lane cleanup failed: {cleanup}")),
        (None, None) => None,
    };
    let status = if reason.is_none() {
        IntegrationLaneStatus::Ready
    } else if cleanup_error.is_some() {
        IntegrationLaneStatus::Failed
    } else {
        classify_lane_failure(reason.as_deref().unwrap_or_default())
    };
    let candidate = candidate.filter(|_| status == IntegrationLaneStatus::Ready);
    let finished_at_unix_ms = unix_time_ms();
    let _ = emit_event(
        &event_sender,
        IntegrationLaneRuntimeEvent::Terminal(IntegrationLaneTerminal {
            plan_id: plan_id.clone(),
            lane_id: lane_id.clone(),
            status,
            candidate: candidate.clone(),
            reason: reason.clone(),
            terminal_at_unix_ms: finished_at_unix_ms,
        }),
    )
    .await;
    debug_assert!(!retained || status == IntegrationLaneStatus::Ready);
    GitIntegrationLaneResult {
        plan_id,
        lane_id,
        status,
        candidate,
        verification_check_ids,
        started_at_unix_ms,
        finished_at_unix_ms,
        reason,
        cleanup_error,
    }
}

async fn execute_managed_ref_lane(
    plan_id: &IntegrationPlanId,
    lane_id: &IntegrationLaneId,
    workspace: &MaterializedGitWorktree,
    artifacts: &[IntegrationArtifact],
    target: &IntegrationLaneTarget,
    event_sender: &Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
) -> Result<IntegrationLaneCandidate> {
    let IntegrationLaneTarget::ManagedRef {
        base_commit,
        expected_oid,
        private_ref,
    } = target
    else {
        bail!("managed-ref lane received a snapshot target");
    };
    let root = workspace.workspace_root();
    let mut expected_old_oid = expected_oid.clone();
    for (index, artifact) in artifacts.iter().enumerate() {
        preflight_changeset_state(root, &artifact.change_set, false).await?;
        apply_patch(root, artifact, true).await?;
        preflight_changeset_state(root, &artifact.change_set, true).await?;
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
        commit_integration_member(root, &artifact.change_set).await?;
        let commit = git_text(
            root,
            [
                OsString::from("rev-parse"),
                OsString::from("--verify"),
                OsString::from("HEAD^{commit}"),
            ],
        )
        .await?;
        advance_private_ref(root, private_ref, &expected_old_oid, &commit).await?;
        let snapshot_id = workspace
            .current_snapshot_id()
            .await
            .context("failed to capture managed-ref lane snapshot")?;
        emit_event(
            event_sender,
            IntegrationLaneRuntimeEvent::MemberApplied(IntegrationLaneMemberApplied {
                plan_id: plan_id.clone(),
                lane_id: lane_id.clone(),
                change_set_id: artifact.change_set.id.clone(),
                member_index: u32::try_from(index)
                    .context("integration lane member index exceeds u32")?,
                effect: IntegrationLaneMemberEffect::ManagedRefAdvanced {
                    expected_old_oid,
                    new_oid: commit.clone(),
                    candidate_snapshot_id: snapshot_id,
                },
                applied_at_unix_ms: unix_time_ms(),
            }),
        )
        .await?;
        expected_old_oid = commit;
    }
    let snapshot_id = workspace
        .current_snapshot_id()
        .await
        .context("failed to capture verified managed-ref lane snapshot")?;
    Ok(IntegrationLaneCandidate::ManagedRef {
        private_ref: private_ref.clone(),
        base_commit: base_commit.clone(),
        candidate_commit: expected_old_oid,
        workspace_snapshot_id: snapshot_id,
    })
}

async fn execute_snapshot_workspace_lane(
    plan_id: &IntegrationPlanId,
    lane_id: &IntegrationLaneId,
    workspace: &MaterializedGitWorktree,
    artifacts: &[IntegrationArtifact],
    target: &IntegrationLaneTarget,
    event_sender: &Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
) -> Result<IntegrationLaneCandidate> {
    let IntegrationLaneTarget::SnapshotWorkspace {
        base_snapshot_id,
        overlay_digest,
        revision,
        owned_workspace_id,
    } = target
    else {
        bail!("snapshot-workspace lane received a managed-ref target");
    };
    let root = workspace.workspace_root();
    let mut expected_snapshot_id = workspace.current_snapshot_id().await?;
    if &expected_snapshot_id != base_snapshot_id {
        bail!(
            "snapshot integration base drifted: expected {base_snapshot_id}, observed {expected_snapshot_id}"
        );
    }
    let mut expected_revision = *revision;
    for (index, artifact) in artifacts.iter().enumerate() {
        let observed_snapshot_id = workspace.current_snapshot_id().await?;
        if observed_snapshot_id != expected_snapshot_id {
            bail!(
                "snapshot integration CAS is stale: expected {expected_snapshot_id}, observed {observed_snapshot_id}"
            );
        }
        preflight_changeset_state(root, &artifact.change_set, false).await?;
        apply_patch(root, artifact, false).await?;
        preflight_changeset_state(root, &artifact.change_set, true).await?;
        let candidate_snapshot_id = workspace.current_snapshot_id().await?;
        let candidate_revision = expected_revision.saturating_add(1);
        emit_event(
            event_sender,
            IntegrationLaneRuntimeEvent::MemberApplied(IntegrationLaneMemberApplied {
                plan_id: plan_id.clone(),
                lane_id: lane_id.clone(),
                change_set_id: artifact.change_set.id.clone(),
                member_index: u32::try_from(index)
                    .context("integration lane member index exceeds u32")?,
                effect: IntegrationLaneMemberEffect::SnapshotWorkspaceApplied {
                    expected_snapshot_id: expected_snapshot_id.clone(),
                    expected_revision,
                    candidate_snapshot_id: candidate_snapshot_id.clone(),
                    candidate_revision,
                },
                applied_at_unix_ms: unix_time_ms(),
            }),
        )
        .await?;
        expected_snapshot_id = candidate_snapshot_id;
        expected_revision = candidate_revision;
    }
    Ok(IntegrationLaneCandidate::SnapshotWorkspace {
        owned_workspace_id: owned_workspace_id.clone(),
        base_snapshot_id: base_snapshot_id.clone(),
        overlay_digest: overlay_digest.clone(),
        revision: expected_revision,
        candidate_snapshot_id: expected_snapshot_id,
    })
}

async fn verify_lane_candidate(
    plan_id: &IntegrationPlanId,
    lane_id: &IntegrationLaneId,
    workspace: &MaterializedGitWorktree,
    artifacts: &[IntegrationArtifact],
    target: &IntegrationLaneTarget,
    candidate: &IntegrationLaneCandidate,
    verification_scope_hashes: &[String],
    execution_backend: &dyn ExecutionBackend,
) -> Result<Vec<VerificationReceipt>> {
    if verification_scope_hashes.is_empty() {
        bail!("integration lane verification requires at least one scope");
    }
    let mut paths = artifacts
        .iter()
        .flat_map(|artifact| {
            artifact.change_set.files.iter().flat_map(|file| {
                std::iter::once(file.path.clone()).chain(file.previous_path.clone())
            })
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        bail!("integration lane verification scope has no changed paths");
    }
    let command_args = match (target, candidate) {
        (
            IntegrationLaneTarget::ManagedRef { base_commit, .. },
            IntegrationLaneCandidate::ManagedRef {
                candidate_commit, ..
            },
        ) => vec![
            "diff".to_owned(),
            "--check".to_owned(),
            base_commit.clone(),
            candidate_commit.clone(),
            "--".to_owned(),
        ],
        (
            IntegrationLaneTarget::SnapshotWorkspace { .. },
            IntegrationLaneCandidate::SnapshotWorkspace { .. },
        ) => vec![
            "diff".to_owned(),
            "--check".to_owned(),
            workspace.baseline_tree().to_owned(),
            "--".to_owned(),
        ],
        _ => bail!("integration lane candidate kind does not match its prepared target"),
    };
    let candidate_snapshot_id = integration_candidate_snapshot_id(candidate);
    let trust_snapshot_id = stable_event_uuid(
        "sigil-integration-verification-trust",
        &format!("{}:{}", plan_id.as_str(), lane_id.as_str()),
    );
    let mut receipts = Vec::with_capacity(verification_scope_hashes.len());
    for scope_hash in verification_scope_hashes {
        let check_spec_id = format!(
            "integration-{}",
            stable_event_uuid(
                "sigil-integration-verification-check",
                &format!("{}:{}:{}", plan_id.as_str(), lane_id.as_str(), scope_hash),
            )
        );
        let command = CheckCommand {
            command: "git".to_owned(),
            args: command_args.clone(),
            cwd: None,
        };
        let check_spec = CheckSpec::new(
            check_spec_id,
            command,
            ToolEffect::ReadOnly,
            scope_hash.clone(),
        );
        let mut verification_scope = VerificationScope::all_tracked(scope_hash.clone());
        verification_scope.include = paths.clone();
        verification_scope.exclude.clear();
        verification_scope.tracked_files_only = false;
        verification_scope.max_file_bytes = MAX_INTEGRATION_ARTIFACT_BYTES as u64;
        let policy = VerificationPolicy {
            required_checks: vec![check_spec.clone()],
            completion_criteria: CompletionCriteria::AllRequiredChecks,
            verification_scope,
            sandbox_profile: SandboxProfileRequirement::None,
            workspace_trust_requirement: sigil_kernel::WorkspaceTrustRequirement::None,
            allow_unverified_completion: false,
            timeout_ms: Some(60_000),
            auto_run: VerificationAutoRunPolicy::Manual,
        };
        let policy_hash = policy.stable_hash()?;
        let trusted_check = TrustedCheckSpec {
            check_spec,
            source: CheckDiscoverySource::RuntimeStructural,
            workspace_trust_snapshot_id: trust_snapshot_id.clone(),
            promoted_by: CheckPromotion::GlobalPolicy {
                policy_event_id: stable_event_uuid(
                    "sigil-integration-verification-policy",
                    &format!("{}:{}", plan_id.as_str(), scope_hash),
                ),
            },
            approval_event_id: None,
            sandbox_decision_id: None,
        };
        let mut verification_session = Session::new("sigil-runtime", "integration-verification-v1");
        let recorded = run_verification_check(
            &mut verification_session,
            execution_backend,
            VerificationCheckRunRequest {
                workspace_root: workspace.workspace_root().to_path_buf(),
                scope: EvidenceScope::Task(format!(
                    "integration:{}:{}",
                    plan_id.as_str(),
                    lane_id.as_str()
                )),
                trusted_check,
                policy,
                policy_hash: Some(policy_hash),
                workspace_trust: WorkspaceTrust::Unknown,
                workspace_trust_snapshot_id: trust_snapshot_id.clone(),
                workspace_trust_approval_event_id: None,
                workspace_trust_sandbox_decision_id: None,
            },
        )
        .await
        .context("failed to execute scoped integration verification")?;
        let receipt = recorded.receipt;
        if receipt.check_status != ReceiptStatus::Succeeded
            || receipt.receipt.status != ReceiptStatus::Succeeded
            || receipt.mutates_verification_scope
        {
            bail!(
                "scoped integration verification {} did not produce immutable succeeded evidence",
                receipt.check_spec_id
            );
        }
        receipts.push(receipt);
    }
    let observed_snapshot_id = workspace
        .current_snapshot_id()
        .await
        .context("failed to revalidate integration candidate after verification")?;
    if observed_snapshot_id != candidate_snapshot_id {
        bail!(
            "integration verification mutated candidate snapshot: expected {candidate_snapshot_id}, observed {observed_snapshot_id}"
        );
    }
    Ok(receipts)
}

fn integration_candidate_snapshot_id(candidate: &IntegrationLaneCandidate) -> &str {
    match candidate {
        IntegrationLaneCandidate::ManagedRef {
            workspace_snapshot_id,
            ..
        } => workspace_snapshot_id,
        IntegrationLaneCandidate::SnapshotWorkspace {
            candidate_snapshot_id,
            ..
        } => candidate_snapshot_id,
    }
}

async fn advance_private_ref(
    root: &Path,
    reference: &str,
    expected_old_oid: &str,
    new_oid: &str,
) -> Result<()> {
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
    let observed_oid = observed.as_deref().unwrap_or(ZERO_GIT_OBJECT_ID);
    if observed_oid != expected_old_oid {
        bail!(
            "private integration ref {reference} is stale: expected {expected_old_oid}, observed {observed_oid}"
        );
    }
    run_git_bytes(
        root,
        [
            OsString::from("update-ref"),
            OsString::from(reference),
            OsString::from(new_oid),
            OsString::from(expected_old_oid),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    .with_context(|| format!("failed to CAS private integration ref {reference}"))?;
    Ok(())
}

async fn materialize_lane_workspace(
    request: &GitIntegrationRunRequest,
    workspace_id: String,
) -> Result<MaterializedGitWorktree> {
    match &request.plan.base_representation {
        IntegrationBaseRepresentation::CleanCommit { .. } => {
            materialize_git_worktree(GitWorktreeMaterializationRequest {
                parent_workspace_root: request.parent_workspace_root.clone(),
                isolated_workspace_id: workspace_id,
                base_snapshot_id: request.plan.base_snapshot_id.clone(),
            })
            .await
        }
        IntegrationBaseRepresentation::SnapshotWorkspace { .. } => {
            materialize_git_worktree_from_frozen_base(
                request.frozen_base.as_ref().ok_or_else(|| {
                    anyhow!("snapshot integration requires a frozen overlay base")
                })?,
                workspace_id,
            )
            .await
        }
        IntegrationBaseRepresentation::Unknown => {
            bail!("physical integration requires a complete base representation")
        }
    }
}

fn validate_physical_base(request: &GitIntegrationRunRequest) -> Result<()> {
    match (&request.plan.base_representation, &request.frozen_base) {
        (IntegrationBaseRepresentation::CleanCommit { .. }, None) => Ok(()),
        (IntegrationBaseRepresentation::CleanCommit { .. }, Some(_)) => {
            bail!("clean integration must not carry a frozen overlay base")
        }
        (
            IntegrationBaseRepresentation::SnapshotWorkspace {
                base_commit,
                overlay_digest,
            },
            Some(frozen),
        ) if frozen.base_snapshot_id() == request.plan.base_snapshot_id
            && frozen.base_commit() == base_commit
            && frozen.overlay_digest() == overlay_digest =>
        {
            Ok(())
        }
        (IntegrationBaseRepresentation::SnapshotWorkspace { .. }, Some(_)) => {
            bail!("snapshot integration frozen base does not match the planned representation")
        }
        (IntegrationBaseRepresentation::SnapshotWorkspace { .. }, None) => {
            bail!("snapshot integration requires a frozen overlay base")
        }
        (IntegrationBaseRepresentation::Unknown, _) => {
            bail!("physical integration requires a complete base representation")
        }
    }
}

fn validate_materialized_base(
    plan: &IntegrationPlan,
    workspace: &MaterializedGitWorktree,
) -> Result<()> {
    match &plan.base_representation {
        IntegrationBaseRepresentation::CleanCommit { base_commit } => {
            if workspace.base_commit() != base_commit || workspace.overlay_digest().is_some() {
                bail!(
                    "integration base commit drifted: expected {base_commit}, observed {}",
                    workspace.base_commit()
                );
            }
        }
        IntegrationBaseRepresentation::SnapshotWorkspace {
            base_commit,
            overlay_digest,
        } => {
            if workspace.base_commit() != base_commit
                || workspace.overlay_digest() != Some(overlay_digest.as_str())
            {
                bail!("snapshot integration materialization drifted from the planned overlay");
            }
        }
        IntegrationBaseRepresentation::Unknown => {
            bail!("physical integration requires a complete base representation");
        }
    }
    Ok(())
}

fn lane_target(
    plan: &IntegrationPlan,
    lane_id: &IntegrationLaneId,
    workspace: &MaterializedGitWorktree,
) -> Result<IntegrationLaneTarget> {
    match &plan.base_representation {
        IntegrationBaseRepresentation::CleanCommit { base_commit } => {
            Ok(IntegrationLaneTarget::ManagedRef {
                base_commit: base_commit.clone(),
                expected_oid: ZERO_GIT_OBJECT_ID.to_owned(),
                private_ref: integration_ref(&plan.plan_id, lane_id),
            })
        }
        IntegrationBaseRepresentation::SnapshotWorkspace { overlay_digest, .. } => {
            Ok(IntegrationLaneTarget::SnapshotWorkspace {
                base_snapshot_id: workspace.child_snapshot_id().to_owned(),
                overlay_digest: overlay_digest.clone(),
                revision: 0,
                owned_workspace_id: workspace.isolated_workspace_id().to_owned(),
            })
        }
        IntegrationBaseRepresentation::Unknown => {
            bail!("physical integration requires a complete base representation")
        }
    }
}

async fn apply_patch(root: &Path, artifact: &IntegrationArtifact, index: bool) -> Result<()> {
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
    let mut args = vec![OsString::from("apply")];
    if index {
        args.push(OsString::from("--index"));
    }
    args.extend([
        OsString::from("--whitespace=error-all"),
        OsString::from("-"),
    ]);
    run_git_bytes_with_stdin(
        root,
        args,
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
    Ok(())
}

async fn commit_integration_member(root: &Path, change_set: &ChangeSet) -> Result<()> {
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
            OsString::from(format!("sigil integration {}", change_set.id.as_str())),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    .context("failed to commit verified integration lane member")?;
    Ok(())
}

async fn preflight_changeset_state(root: &Path, change_set: &ChangeSet, after: bool) -> Result<()> {
    for file in &change_set.files {
        match (file.action, after) {
            (ChangeSetFileAction::Create, false) => require_path_absent(root, &file.path).await?,
            (ChangeSetFileAction::Create | ChangeSetFileAction::Update, true) => {
                require_path_hash(root, &file.path, file.after_hash.as_deref()).await?;
            }
            (ChangeSetFileAction::Update | ChangeSetFileAction::Delete, false) => {
                require_path_hash(root, &file.path, file.before_hash.as_deref()).await?;
            }
            (ChangeSetFileAction::Delete, true) => require_path_absent(root, &file.path).await?,
            (ChangeSetFileAction::Rename, false) => {
                let previous_path = file
                    .previous_path
                    .as_deref()
                    .ok_or_else(|| anyhow!("rename changeset is missing its previous path"))?;
                require_path_hash(root, previous_path, file.before_hash.as_deref()).await?;
                require_path_absent(root, &file.path).await?;
            }
            (ChangeSetFileAction::Rename, true) => {
                let previous_path = file
                    .previous_path
                    .as_deref()
                    .ok_or_else(|| anyhow!("rename changeset is missing its previous path"))?;
                require_path_absent(root, previous_path).await?;
                require_path_hash(root, &file.path, file.after_hash.as_deref()).await?;
            }
        }
    }
    Ok(())
}

async fn require_path_absent(root: &Path, relative_path: &str) -> Result<()> {
    let path = root.join(relative_path);
    match tokio::fs::symlink_metadata(&path).await {
        Ok(_) => bail!(
            "integration preflight expected {} to be absent",
            relative_path
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect integration path {relative_path}")),
    }
}

async fn require_path_hash(
    root: &Path,
    relative_path: &str,
    expected_hash: Option<&str>,
) -> Result<()> {
    let expected_hash = expected_hash
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("integration path {relative_path} is missing its expected hash"))?;
    let path = root.join(relative_path);
    let metadata = tokio::fs::symlink_metadata(&path)
        .await
        .with_context(|| format!("failed to inspect integration path {relative_path}"))?;
    if !metadata.file_type().is_file() {
        bail!("integration path {relative_path} is not a regular file");
    }
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("failed to read integration path {relative_path}"))?;
    if bytes.len() > MAX_INTEGRATION_ARTIFACT_BYTES {
        bail!("integration path {relative_path} exceeds the lane file budget");
    }
    let observed_hash = format!("{:x}", Sha256::digest(&bytes));
    if observed_hash != expected_hash {
        bail!(
            "integration path {relative_path} hash mismatch: expected {expected_hash}, observed {observed_hash}"
        );
    }
    Ok(())
}

async fn emit_event(
    sender: &Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
    event: IntegrationLaneRuntimeEvent,
) -> Result<()> {
    if let Some(sender) = sender {
        let (acknowledgement, receiver) = oneshot::channel();
        sender
            .send(IntegrationLaneRuntimeEventRequest {
                event,
                acknowledgement,
            })
            .map_err(|_| anyhow!("integration lane durable event receiver closed"))?;
        receiver
            .await
            .map_err(|_| anyhow!("integration lane durable event acknowledgement dropped"))?
            .map_err(|error| anyhow!("integration lane durable event was rejected: {error}"))?;
    }
    Ok(())
}

async fn emit_cleanup_event(
    sender: &Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
    plan_id: &IntegrationPlanId,
    lane_id: &IntegrationLaneId,
    workspace_id: &str,
    status: IntegrationLaneCleanupStatus,
    workspace_status: IsolatedWorkspaceCleanupStatus,
) -> Result<()> {
    emit_event(
        sender,
        IntegrationLaneRuntimeEvent::CleanupRecorded {
            entry: IntegrationLaneCleanupRecorded {
                plan_id: plan_id.clone(),
                lane_id: lane_id.clone(),
                owned_workspace_id: workspace_id.to_owned(),
                status,
                recorded_at_unix_ms: unix_time_ms(),
            },
            workspace_status,
        },
    )
    .await
}

async fn cleanup_lane_workspace(
    sender: &Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
    plan_id: IntegrationPlanId,
    lane_id: IntegrationLaneId,
    workspace: MaterializedGitWorktree,
    _intended_status: IntegrationLaneCleanupStatus,
) {
    let workspace_id = workspace.isolated_workspace_id().to_owned();
    match workspace.cleanup().await {
        Ok(receipt) => {
            let _ = emit_cleanup_event(
                sender,
                &plan_id,
                &lane_id,
                &workspace_id,
                cleanup_status_from_workspace(receipt.status),
                receipt.status,
            )
            .await;
        }
        Err(_) => {
            let _ = emit_cleanup_event(
                sender,
                &plan_id,
                &lane_id,
                &workspace_id,
                IntegrationLaneCleanupStatus::Failed,
                IsolatedWorkspaceCleanupStatus::Failed,
            )
            .await;
        }
    }
}

async fn cleanup_failed_preparation(
    sender: &Option<UnboundedSender<IntegrationLaneRuntimeEventRequest>>,
    plan_id: IntegrationPlanId,
    lane_id: IntegrationLaneId,
    workspace: MaterializedGitWorktree,
    materialized: Vec<MaterializedIntegrationLane>,
) {
    cleanup_lane_workspace(
        sender,
        plan_id,
        lane_id,
        workspace,
        IntegrationLaneCleanupStatus::Removed,
    )
    .await;
    for (plan_id, lane_id, workspace, _, _, _) in materialized {
        cleanup_lane_workspace(
            sender,
            plan_id,
            lane_id,
            workspace,
            IntegrationLaneCleanupStatus::Removed,
        )
        .await;
    }
}

fn cleanup_status_from_workspace(
    status: IsolatedWorkspaceCleanupStatus,
) -> IntegrationLaneCleanupStatus {
    match status {
        IsolatedWorkspaceCleanupStatus::Removed => IntegrationLaneCleanupStatus::Removed,
        IsolatedWorkspaceCleanupStatus::AlreadyMissing => {
            IntegrationLaneCleanupStatus::AlreadyMissing
        }
        IsolatedWorkspaceCleanupStatus::Retained => IntegrationLaneCleanupStatus::Retained,
        IsolatedWorkspaceCleanupStatus::Failed => IntegrationLaneCleanupStatus::Failed,
    }
}

fn validate_integration_request(request: &GitIntegrationRunRequest) -> Result<()> {
    if request.plan.lanes.is_empty() {
        bail!("physical integration requires at least one lane");
    }
    for proposal in &request.plan.proposals {
        proposal.validate()?;
    }
    if request.plan.requires_manual_review() {
        bail!("physical integration requires complete proposal and base facts");
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
        let proposal = request
            .plan
            .proposals
            .iter()
            .find(|proposal| proposal.change_set_id == artifact.change_set.id)
            .ok_or_else(|| {
                anyhow!(
                    "integration artifact {} has no planned proposal",
                    artifact.change_set.id.as_str()
                )
            })?;
        validate_artifact_matches_proposal(artifact, proposal)?;
    }
    if expected != observed {
        bail!("physical integration artifacts do not exactly match the planned proposals");
    }
    Ok(())
}

fn validate_artifact_matches_proposal(
    artifact: &IntegrationArtifact,
    proposal: &sigil_kernel::IntegrationProposalSpec,
) -> Result<()> {
    let artifact_files = artifact
        .change_set
        .files
        .iter()
        .map(|file| {
            (
                file.path.as_str(),
                file.previous_path.as_deref(),
                file.action,
                file.before_hash.as_deref(),
                file.after_hash.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    let fact_files = proposal
        .facts
        .paths
        .iter()
        .map(|file| {
            (
                file.path.as_str(),
                file.previous_path.as_deref(),
                file.action,
                file.before_hash.as_deref(),
                file.after_hash.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    if artifact_files != fact_files {
        bail!(
            "integration artifact {} does not match its content-bound path facts",
            artifact.change_set.id.as_str()
        );
    }
    Ok(())
}

fn classify_lane_failure(reason: &str) -> IntegrationLaneStatus {
    if reason.contains("parent workspace snapshot drifted")
        || reason.contains("snapshot integration CAS is stale")
        || reason.contains("private integration ref") && reason.contains("stale")
    {
        IntegrationLaneStatus::Stale
    } else if reason.contains("integration conflict")
        || reason.contains("patch failed")
        || reason.contains("hash mismatch")
        || reason.contains("expected") && reason.contains("to be absent")
    {
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
