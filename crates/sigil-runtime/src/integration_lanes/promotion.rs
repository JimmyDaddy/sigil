use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::PathBuf,
};

use anyhow::{Context, Result, anyhow, bail};
use sigil_kernel::{
    ChangeSetId, IntegrationBaseRepresentation, IntegrationPromotionAttemptId,
    IntegrationPromotionEffect, IntegrationPromotionRecorded, IntegrationPromotionStatus,
    IntegrationPromotionTarget, MutationEventRecorder, ParentChangeSetMutationRequest,
    TaskChildChangeSetProposal, TaskPromotionAuthority, TaskPromotionAuthorityConsumed,
    TaskPromotionPreview, apply_parent_changeset_mutation_batch, stable_event_uuid,
};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use super::{
    GitIntegrationRunRequest, IntegrationArtifact, MAX_INTEGRATION_GIT_OUTPUT_BYTES, apply_patch,
    git_optional_text, git_text, materialize_lane_workspace, preflight_changeset_state,
    run_git_bytes, unix_time_ms, validate_integration_request, validate_physical_base,
};
use crate::isolated_workspace::{FrozenGitWorktreeBase, MaterializedGitWorktree};

/// Host-selected target kind used while materializing one exact aggregate promotion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationPromotionPreparationTarget {
    WorkspaceApply {
        expected_snapshot_id: String,
        expected_revision: u64,
    },
    GitRefAdvance {
        target_ref: String,
        expected_old_oid: String,
    },
}

/// Request to rebuild one aggregate candidate from the exact planned proposal artifacts.
#[derive(Clone)]
pub struct GitIntegrationPromotionPreparationRequest {
    pub preparation_id: String,
    pub parent_workspace_root: PathBuf,
    pub plan: sigil_kernel::IntegrationPlan,
    pub artifacts: Vec<IntegrationArtifact>,
    pub frozen_base: Option<FrozenGitWorktreeBase>,
    pub target: IntegrationPromotionPreparationTarget,
}

/// Runtime-owned aggregate diff and private workspace retained until review resolution.
#[derive(Debug)]
pub struct PreparedGitIntegrationPromotion {
    plan_id: sigil_kernel::IntegrationPlanId,
    workspace: MaterializedGitWorktree,
    aggregate: TaskChildChangeSetProposal,
    target: IntegrationPromotionTarget,
}

impl PreparedGitIntegrationPromotion {
    #[must_use]
    pub fn plan_id(&self) -> &sigil_kernel::IntegrationPlanId {
        &self.plan_id
    }

    #[must_use]
    pub fn aggregate(&self) -> &TaskChildChangeSetProposal {
        &self.aggregate
    }

    #[must_use]
    pub fn aggregate_diff_digest(&self) -> String {
        format!("sha256:{}", self.aggregate.artifact.content_sha256)
    }

    #[must_use]
    pub fn target(&self) -> &IntegrationPromotionTarget {
        &self.target
    }

    #[must_use]
    pub fn candidate_snapshot_id(&self) -> Option<&str> {
        self.aggregate.child_snapshot_id.as_deref()
    }

    #[must_use]
    pub fn owned_workspace_id(&self) -> &str {
        self.workspace.isolated_workspace_id()
    }

    /// Removes the exact runtime-owned candidate workspace after a denied or abandoned review.
    ///
    /// # Errors
    ///
    /// Returns an error when Git cannot remove this exact owned worktree.
    pub async fn cleanup(self) -> Result<()> {
        self.workspace.cleanup().await?;
        Ok(())
    }
}

/// Recovery-critical event emitted at the final promotion boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationPromotionRuntimeEvent {
    AuthorityConsumed(TaskPromotionAuthorityConsumed),
    PromotionRecorded(IntegrationPromotionRecorded),
}

/// One promotion event plus a required durable acknowledgement.
#[derive(Debug)]
pub struct IntegrationPromotionRuntimeEventRequest {
    event: IntegrationPromotionRuntimeEvent,
    acknowledgement: oneshot::Sender<std::result::Result<(), String>>,
}

impl IntegrationPromotionRuntimeEventRequest {
    #[must_use]
    pub fn event(&self) -> &IntegrationPromotionRuntimeEvent {
        &self.event
    }

    pub fn acknowledge(self, result: std::result::Result<(), String>) {
        let _ = self.acknowledgement.send(result);
    }
}

/// Exact authorized promotion request. The prepared candidate is consumed once.
pub struct GitIntegrationPromotionRunRequest {
    pub prepared: PreparedGitIntegrationPromotion,
    pub attempt_id: IntegrationPromotionAttemptId,
    pub preview: TaskPromotionPreview,
    pub authority: TaskPromotionAuthority,
    pub mutation_recorder: MutationEventRecorder,
}

/// Terminal physical promotion result plus the authoritative snapshot used by parent checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIntegrationPromotionOutput {
    pub record: IntegrationPromotionRecorded,
    pub authoritative_snapshot_id: Option<String>,
    pub cleanup_error: Option<String>,
}

/// Materializes all accepted lane artifacts into one aggregate candidate.
///
/// Every artifact is replayed exactly once in deterministic lane/member order against the same
/// frozen base used by the lanes. The resulting full-base diff is the only artifact later allowed
/// to mutate the parent workspace. Git-ref targets additionally receive one candidate commit, but
/// no user ref is advanced during preparation.
///
/// # Errors
///
/// Returns an error for invalid plan/artifact/base bindings, apply conflict, target mismatch, an
/// empty aggregate diff, or cleanup failure after an unsuccessful preparation.
pub async fn prepare_git_integration_promotion(
    request: GitIntegrationPromotionPreparationRequest,
) -> Result<PreparedGitIntegrationPromotion> {
    validate_preparation_id(&request.preparation_id)?;
    let lane_request = GitIntegrationRunRequest {
        parent_workspace_root: request.parent_workspace_root.clone(),
        plan: request.plan.clone(),
        artifacts: request.artifacts.clone(),
        frozen_base: request.frozen_base.clone(),
        verification_backend: None,
    };
    validate_integration_request(&lane_request)?;
    validate_physical_base(&lane_request)?;
    let workspace_id = promotion_workspace_id(&request.plan.plan_id, &request.preparation_id);
    let workspace = materialize_lane_workspace(&lane_request, workspace_id)
        .await
        .context("failed to materialize aggregate promotion workspace")?;
    match prepare_in_workspace(&request, &workspace).await {
        Ok((aggregate, target)) => Ok(PreparedGitIntegrationPromotion {
            plan_id: request.plan.plan_id,
            workspace,
            aggregate,
            target,
        }),
        Err(error) => match workspace.cleanup().await {
            Ok(_) => Err(error),
            Err(cleanup) => Err(error.context(format!(
                "aggregate promotion workspace cleanup also failed: {cleanup:#}"
            ))),
        },
    }
}

async fn prepare_in_workspace(
    request: &GitIntegrationPromotionPreparationRequest,
    workspace: &MaterializedGitWorktree,
) -> Result<(TaskChildChangeSetProposal, IntegrationPromotionTarget)> {
    let artifacts = request
        .artifacts
        .iter()
        .map(|artifact| (artifact.change_set.id.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut applied = BTreeSet::new();
    for lane in &request.plan.lanes {
        for change_set_id in &lane.proposals {
            if !applied.insert(change_set_id.clone()) {
                bail!(
                    "aggregate promotion plan repeats changeset {}",
                    change_set_id.as_str()
                );
            }
            let artifact = artifacts.get(change_set_id).ok_or_else(|| {
                anyhow!(
                    "aggregate promotion is missing changeset {}",
                    change_set_id.as_str()
                )
            })?;
            preflight_changeset_state(workspace.workspace_root(), &artifact.change_set, false)
                .await?;
            apply_patch(workspace.workspace_root(), artifact, false).await?;
            preflight_changeset_state(workspace.workspace_root(), &artifact.change_set, true)
                .await?;
        }
    }
    if applied.len() != artifacts.len() {
        bail!("aggregate promotion did not consume every planned artifact");
    }
    run_git_bytes(
        workspace.workspace_root(),
        [
            OsString::from("diff"),
            OsString::from("--check"),
            OsString::from(workspace.baseline_tree()),
            OsString::from("--"),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    .context("aggregate promotion diff verification failed")?;
    let change_set_id = ChangeSetId::new(format!(
        "promotion-{}",
        stable_event_uuid(
            "sigil-integration-promotion-changeset",
            &format!(
                "{}:{}",
                request.plan.plan_id.as_str(),
                request.preparation_id
            ),
        )
    ))?;
    let aggregate = workspace
        .extract_changeset(
            change_set_id,
            "Task integration promotion",
            format!("Aggregate {} integration lane(s)", request.plan.lanes.len()),
        )
        .await?
        .ok_or_else(|| anyhow!("aggregate promotion produced no changes"))?;
    let target = match &request.target {
        IntegrationPromotionPreparationTarget::WorkspaceApply {
            expected_snapshot_id,
            expected_revision,
        } => {
            if expected_snapshot_id != &request.plan.base_snapshot_id {
                bail!("workspace promotion target does not match the integration base snapshot");
            }
            IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id: expected_snapshot_id.clone(),
                expected_revision: *expected_revision,
            }
        }
        IntegrationPromotionPreparationTarget::GitRefAdvance {
            target_ref,
            expected_old_oid,
        } => {
            let IntegrationBaseRepresentation::CleanCommit { base_commit } =
                &request.plan.base_representation
            else {
                bail!("Git ref promotion requires a clean commit base");
            };
            if expected_old_oid != base_commit {
                bail!("Git ref promotion expected oid does not match the integration base");
            }
            validate_target_ref(workspace.workspace_root(), target_ref).await?;
            run_git_bytes(
                workspace.workspace_root(),
                [
                    OsString::from("add"),
                    OsString::from("--all"),
                    OsString::from("--"),
                ],
                MAX_INTEGRATION_GIT_OUTPUT_BYTES,
            )
            .await
            .context("failed to stage aggregate promotion candidate")?;
            run_git_bytes(
                workspace.workspace_root(),
                [
                    OsString::from("-c"),
                    OsString::from("user.name=Sigil Integration"),
                    OsString::from("-c"),
                    OsString::from("user.email=sigil-integration@localhost"),
                    OsString::from("commit"),
                    OsString::from("--quiet"),
                    OsString::from("-m"),
                    OsString::from(format!("sigil promotion {}", request.plan.plan_id.as_str())),
                ],
                MAX_INTEGRATION_GIT_OUTPUT_BYTES,
            )
            .await
            .context("failed to commit aggregate promotion candidate")?;
            let candidate_oid = git_text(
                workspace.workspace_root(),
                [
                    OsString::from("rev-parse"),
                    OsString::from("--verify"),
                    OsString::from("HEAD^{commit}"),
                ],
            )
            .await?;
            IntegrationPromotionTarget::GitRefAdvance {
                target_ref: target_ref.clone(),
                expected_old_oid: expected_old_oid.clone(),
                candidate_oid,
            }
        }
    };
    Ok((aggregate, target))
}

#[cfg(test)]
pub(crate) async fn run_git_integration_promotion(
    request: GitIntegrationPromotionRunRequest,
) -> Result<GitIntegrationPromotionOutput> {
    run_git_integration_promotion_inner(request, None).await
}

/// Executes an exact promotion after durable authority and prepared acknowledgements.
///
/// WorkspaceApply uses the RFC-0002 aggregate mutation batch and never advances a ref.
/// GitRefAdvance requires a clean repository, rejects a checked-out target ref, and performs one
/// compare-and-swap ref update without touching the user worktree.
///
/// # Errors
///
/// Returns an error before effect for stale/mismatched authority or when required durable event
/// acknowledgement fails. A receiver failure after the physical effect leaves a Prepared fact;
/// recovery must inspect but never replay that unknown effect.
pub async fn run_git_integration_promotion_with_events(
    request: GitIntegrationPromotionRunRequest,
    event_sender: UnboundedSender<IntegrationPromotionRuntimeEventRequest>,
) -> Result<GitIntegrationPromotionOutput> {
    run_git_integration_promotion_inner(request, Some(event_sender)).await
}

async fn run_git_integration_promotion_inner(
    request: GitIntegrationPromotionRunRequest,
    event_sender: Option<UnboundedSender<IntegrationPromotionRuntimeEventRequest>>,
) -> Result<GitIntegrationPromotionOutput> {
    if let Err(error) = validate_run_request(&request) {
        return cleanup_rejected_run(request, error).await;
    }
    let consumed_at_unix_ms = unix_time_ms();
    if let Err(error) = request
        .authority
        .validate_for_preview(&request.preview, consumed_at_unix_ms)
    {
        return cleanup_rejected_run(request, error).await;
    }
    if let Err(error) = emit_promotion_event(
        &event_sender,
        IntegrationPromotionRuntimeEvent::AuthorityConsumed(TaskPromotionAuthorityConsumed {
            attempt_id: request.attempt_id.clone(),
            authority: request.authority.clone(),
            consumed_at_unix_ms,
        }),
    )
    .await
    {
        return cleanup_rejected_run(request, error).await;
    }
    let prepared_record = promotion_record(
        &request,
        IntegrationPromotionStatus::Prepared,
        None,
        None,
        consumed_at_unix_ms,
    );
    if let Err(error) = emit_promotion_event(
        &event_sender,
        IntegrationPromotionRuntimeEvent::PromotionRecorded(prepared_record),
    )
    .await
    {
        return cleanup_rejected_run(request, error).await;
    }

    let GitIntegrationPromotionRunRequest {
        prepared,
        attempt_id,
        preview,
        authority,
        mutation_recorder,
    } = request;
    let physical = match &preview.target {
        IntegrationPromotionTarget::WorkspaceApply {
            expected_snapshot_id,
            expected_revision,
        } => {
            let recorder = mutation_recorder.clone();
            let mutation_request = ParentChangeSetMutationRequest {
                operation_key: attempt_id.as_str().to_owned(),
                expected_workspace_snapshot_id: expected_snapshot_id.clone(),
                change_set: prepared.aggregate.change_set.clone(),
                artifact_content: prepared.aggregate.artifact.content.clone(),
                artifact_digest: format!("sha256:{}", prepared.aggregate.artifact.content_sha256),
                workspace_root: prepared.workspace.parent_workspace_root().to_path_buf(),
                tool_call_id: format!("promotion-{}", attempt_id.as_str()),
            };
            let outcome = tokio::task::spawn_blocking(move || {
                apply_parent_changeset_mutation_batch(&recorder, mutation_request)
            })
            .await;
            let outcome = match outcome {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(error)) => {
                    return finalize_promotion(
                        prepared,
                        attempt_id,
                        preview,
                        authority,
                        PhysicalPromotion::terminal(
                            IntegrationPromotionStatus::Failed,
                            format!("aggregate parent mutation failed: {error:#}"),
                        ),
                        &event_sender,
                    )
                    .await;
                }
                Err(error) => {
                    return finalize_promotion(
                        prepared,
                        attempt_id,
                        preview,
                        authority,
                        PhysicalPromotion::terminal(
                            IntegrationPromotionStatus::Failed,
                            format!("aggregate parent mutation task failed: {error:#}"),
                        ),
                        &event_sender,
                    )
                    .await;
                }
            };
            if let Some(reason) = outcome.conflict_reason {
                PhysicalPromotion::terminal(
                    if reason.contains("stale parent workspace snapshot") {
                        IntegrationPromotionStatus::Stale
                    } else {
                        IntegrationPromotionStatus::Conflict
                    },
                    reason,
                )
            } else if outcome.is_applied() {
                let promoted_snapshot_id = outcome
                    .observed_workspace_snapshot_after_id
                    .expect("applied mutation owns a post snapshot");
                PhysicalPromotion {
                    status: IntegrationPromotionStatus::Promoted,
                    effect: Some(IntegrationPromotionEffect::WorkspaceApplied {
                        promoted_snapshot_id: promoted_snapshot_id.clone(),
                        promoted_revision: expected_revision.saturating_add(1),
                    }),
                    authoritative_snapshot_id: Some(promoted_snapshot_id),
                    reason: None,
                }
            } else {
                PhysicalPromotion::terminal(
                    IntegrationPromotionStatus::Failed,
                    format!(
                        "aggregate parent mutation ended with batch status {:?}; committed={}, failed={}",
                        outcome.batch_status,
                        outcome.committed_operations.len(),
                        outcome.failed_operations.len()
                    ),
                )
            }
        }
        IntegrationPromotionTarget::GitRefAdvance {
            target_ref,
            expected_old_oid,
            candidate_oid,
        } => {
            execute_git_ref_advance(
                prepared.workspace.parent_workspace_root(),
                target_ref,
                expected_old_oid,
                candidate_oid,
                prepared
                    .candidate_snapshot_id()
                    .ok_or_else(|| anyhow!("promotion candidate snapshot is missing"))?,
            )
            .await
        }
    };
    finalize_promotion(
        prepared,
        attempt_id,
        preview,
        authority,
        physical,
        &event_sender,
    )
    .await
}

async fn finalize_promotion(
    prepared: PreparedGitIntegrationPromotion,
    attempt_id: IntegrationPromotionAttemptId,
    preview: TaskPromotionPreview,
    authority: TaskPromotionAuthority,
    physical: PhysicalPromotion,
    event_sender: &Option<UnboundedSender<IntegrationPromotionRuntimeEventRequest>>,
) -> Result<GitIntegrationPromotionOutput> {
    let terminal_record = IntegrationPromotionRecorded {
        plan_id: prepared.plan_id.clone(),
        attempt_id: Some(attempt_id),
        status: physical.status,
        preview_digest: preview.preview_digest,
        target: preview.target,
        authority_nonce: Some(authority.nonce),
        effect: physical.effect,
        reason: physical.reason,
        recorded_at_unix_ms: unix_time_ms(),
    };
    let terminal_emit = emit_promotion_event(
        event_sender,
        IntegrationPromotionRuntimeEvent::PromotionRecorded(terminal_record.clone()),
    )
    .await;
    let cleanup_error = prepared
        .workspace
        .cleanup()
        .await
        .err()
        .map(|error| format!("{error:#}"));
    terminal_emit?;
    Ok(GitIntegrationPromotionOutput {
        record: terminal_record,
        authoritative_snapshot_id: physical.authoritative_snapshot_id,
        cleanup_error,
    })
}

async fn cleanup_rejected_run(
    request: GitIntegrationPromotionRunRequest,
    error: anyhow::Error,
) -> Result<GitIntegrationPromotionOutput> {
    match request.prepared.workspace.cleanup().await {
        Ok(_) => Err(error),
        Err(cleanup) => Err(error.context(format!(
            "rejected promotion workspace cleanup also failed: {cleanup:#}"
        ))),
    }
}

struct PhysicalPromotion {
    status: IntegrationPromotionStatus,
    effect: Option<IntegrationPromotionEffect>,
    authoritative_snapshot_id: Option<String>,
    reason: Option<String>,
}

impl PhysicalPromotion {
    fn terminal(status: IntegrationPromotionStatus, reason: String) -> Self {
        Self {
            status,
            effect: None,
            authoritative_snapshot_id: None,
            reason: Some(reason),
        }
    }
}

async fn execute_git_ref_advance(
    parent_workspace_root: &std::path::Path,
    target_ref: &str,
    expected_old_oid: &str,
    candidate_oid: &str,
    candidate_snapshot_id: &str,
) -> PhysicalPromotion {
    match git_ref_preflight(
        parent_workspace_root,
        target_ref,
        expected_old_oid,
        candidate_oid,
    )
    .await
    {
        Ok(()) => {}
        Err(error) => {
            let reason = format!("{error:#}");
            return PhysicalPromotion::terminal(
                if reason.contains("stale") {
                    IntegrationPromotionStatus::Stale
                } else {
                    IntegrationPromotionStatus::Conflict
                },
                reason,
            );
        }
    }
    if let Err(error) = run_git_bytes(
        parent_workspace_root,
        [
            OsString::from("update-ref"),
            OsString::from(target_ref),
            OsString::from(candidate_oid),
            OsString::from(expected_old_oid),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    {
        return PhysicalPromotion::terminal(
            IntegrationPromotionStatus::Stale,
            format!("Git ref promotion CAS failed: {error:#}"),
        );
    }
    PhysicalPromotion {
        status: IntegrationPromotionStatus::Promoted,
        effect: Some(IntegrationPromotionEffect::GitRefAdvanced {
            old_oid: expected_old_oid.to_owned(),
            new_oid: candidate_oid.to_owned(),
        }),
        authoritative_snapshot_id: Some(candidate_snapshot_id.to_owned()),
        reason: None,
    }
}

async fn git_ref_preflight(
    parent_workspace_root: &std::path::Path,
    target_ref: &str,
    expected_old_oid: &str,
    candidate_oid: &str,
) -> Result<()> {
    validate_target_ref(parent_workspace_root, target_ref).await?;
    let status = run_git_bytes(
        parent_workspace_root,
        [
            OsString::from("status"),
            OsString::from("--porcelain=v1"),
            OsString::from("--untracked-files=all"),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await?;
    if !status.is_empty() {
        bail!("Git ref promotion requires a clean repository");
    }
    let worktrees = run_git_bytes(
        parent_workspace_root,
        [
            OsString::from("worktree"),
            OsString::from("list"),
            OsString::from("--porcelain"),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await?;
    let worktrees =
        String::from_utf8(worktrees).context("Git worktree inventory is not valid UTF-8")?;
    if worktrees
        .lines()
        .any(|line| line.strip_prefix("branch ") == Some(target_ref))
    {
        bail!("Git ref promotion target is checked out in a worktree");
    }
    let observed = git_optional_text(
        parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(target_ref),
        ],
    )
    .await?
    .ok_or_else(|| anyhow!("Git ref promotion target is missing"))?;
    if observed != expected_old_oid {
        bail!(
            "Git ref promotion target is stale: expected {expected_old_oid}, observed {observed}"
        );
    }
    run_git_bytes(
        parent_workspace_root,
        [
            OsString::from("merge-base"),
            OsString::from("--is-ancestor"),
            OsString::from(expected_old_oid),
            OsString::from(candidate_oid),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    .context("Git ref promotion candidate does not descend from the expected oid")?;
    Ok(())
}

async fn validate_target_ref(root: &std::path::Path, target_ref: &str) -> Result<()> {
    if !target_ref.starts_with("refs/") {
        bail!("Git ref promotion target must be a fully qualified ref");
    }
    run_git_bytes(
        root,
        [
            OsString::from("check-ref-format"),
            OsString::from(target_ref),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await
    .context("Git ref promotion target is invalid")?;
    Ok(())
}

fn validate_run_request(request: &GitIntegrationPromotionRunRequest) -> Result<()> {
    request.preview.validate()?;
    if request.prepared.plan_id != request.preview.plan_id {
        bail!("prepared promotion belongs to another integration plan");
    }
    if request.prepared.target != request.preview.target {
        bail!("prepared promotion target does not match the exact preview");
    }
    if request.prepared.aggregate.artifact_ref != request.preview.aggregate_diff_artifact_ref
        || request.prepared.aggregate_diff_digest() != request.preview.aggregate_diff_digest
    {
        bail!("prepared promotion aggregate artifact does not match the exact preview");
    }
    request
        .authority
        .validate_for_preview(&request.preview, unix_time_ms())?;
    Ok(())
}

fn promotion_record(
    request: &GitIntegrationPromotionRunRequest,
    status: IntegrationPromotionStatus,
    effect: Option<IntegrationPromotionEffect>,
    reason: Option<String>,
    recorded_at_unix_ms: u64,
) -> IntegrationPromotionRecorded {
    IntegrationPromotionRecorded {
        plan_id: request.preview.plan_id.clone(),
        attempt_id: Some(request.attempt_id.clone()),
        status,
        preview_digest: request.preview.preview_digest.clone(),
        target: request.preview.target.clone(),
        authority_nonce: Some(request.authority.nonce.clone()),
        effect,
        reason,
        recorded_at_unix_ms,
    }
}

async fn emit_promotion_event(
    sender: &Option<UnboundedSender<IntegrationPromotionRuntimeEventRequest>>,
    event: IntegrationPromotionRuntimeEvent,
) -> Result<()> {
    if let Some(sender) = sender {
        let (acknowledgement, receiver) = oneshot::channel();
        sender
            .send(IntegrationPromotionRuntimeEventRequest {
                event,
                acknowledgement,
            })
            .map_err(|_| anyhow!("integration promotion durable event receiver closed"))?;
        receiver
            .await
            .map_err(|_| anyhow!("integration promotion durable event acknowledgement dropped"))?
            .map_err(|error| {
                anyhow!("integration promotion durable event was rejected: {error}")
            })?;
    }
    Ok(())
}

fn promotion_workspace_id(
    plan_id: &sigil_kernel::IntegrationPlanId,
    preparation_id: &str,
) -> String {
    format!(
        "promotion-{}",
        stable_event_uuid(
            "sigil-integration-promotion-workspace",
            &format!("{}:{preparation_id}", plan_id.as_str()),
        )
    )
}

fn validate_preparation_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\\'])
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("integration promotion preparation id is invalid");
    }
    Ok(())
}
