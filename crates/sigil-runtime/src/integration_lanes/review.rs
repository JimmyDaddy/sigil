use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, EvidenceScope, IntegrationBaseRepresentation, IntegrationProjection, Session,
    SessionLogEntry, TaskIntegrationReviewRequest, TaskPromotionPreview, VerificationPolicy,
    stable_event_uuid, stable_workspace_id,
};

use super::{
    GitIntegrationPromotionPreparationRequest, IntegrationArtifact,
    IntegrationPromotionPreparationTarget, PreparedGitIntegrationPromotion,
    prepare_git_integration_promotion,
};
use crate::isolated_workspace::{
    FrozenGitWorktreeBase, FrozenGitWorktreeBaseRestoreRequest, restore_frozen_git_worktree_base,
};

const MAX_REVIEW_CHANGESET_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

/// Reconstructed, exact promotion candidate retained for one accepted product review.
#[derive(Debug)]
pub struct PreparedTaskIntegrationReview {
    pub prepared: PreparedGitIntegrationPromotion,
    pub preview: TaskPromotionPreview,
    pub verification_policy: VerificationPolicy,
}

/// Rebuilds the exact aggregate candidate bound to one current durable review request.
///
/// Proposal and dirty-overlay bytes are read only from the content-addressed mutation artifact
/// store. The rebuilt aggregate must byte-match the reviewed artifact and target before this
/// function returns a candidate that can cross the promotion authority barrier.
///
/// # Errors
///
/// Returns an error for stale review identity, missing/substituted provenance or artifacts,
/// parent drift, aggregate mismatch, or verification-policy drift.
pub async fn prepare_task_integration_review(
    session: &Session,
    workspace_root: &Path,
    request: &TaskIntegrationReviewRequest,
) -> Result<PreparedTaskIntegrationReview> {
    let product = sigil_kernel::task_integration_review_product(session.entries())
        .ok_or_else(|| anyhow!("task integration review is no longer current"))?;
    request.validate_for_preview(&product.preview)?;
    if product.request != *request {
        bail!("task integration review request is stale or substituted");
    }
    let integration = IntegrationProjection::from_entries(session.entries());
    let state = integration
        .plans
        .get(&product.preview.plan_id)
        .ok_or_else(|| anyhow!("integration plan disappeared before review acceptance"))?;
    if state.inconsistent || state.recorded.plan.plan_version != product.preview.plan_version {
        bail!("integration plan changed before review acceptance");
    }
    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| anyhow!("task integration review requires durable artifact storage"))?;
    let artifacts =
        load_review_artifacts(session.entries(), &state.recorded.plan, recorder.clone()).await?;
    let frozen_base =
        restore_review_frozen_base(session.entries(), workspace_root, state, recorder.clone())
            .await?;
    let target = match &product.preview.target {
        sigil_kernel::IntegrationPromotionTarget::WorkspaceApply {
            expected_snapshot_id,
            expected_revision,
        } => IntegrationPromotionPreparationTarget::WorkspaceApply {
            expected_snapshot_id: expected_snapshot_id.clone(),
            expected_revision: *expected_revision,
        },
        sigil_kernel::IntegrationPromotionTarget::GitRefAdvance {
            target_ref,
            expected_old_oid,
            ..
        } => IntegrationPromotionPreparationTarget::GitRefAdvance {
            target_ref: target_ref.clone(),
            expected_old_oid: expected_old_oid.clone(),
        },
    };
    let mut prepared =
        prepare_git_integration_promotion(GitIntegrationPromotionPreparationRequest {
            preparation_id: format!(
                "accepted-{}",
                stable_event_uuid("sigil-task-integration-review", &request.request_id)
            ),
            parent_workspace_root: workspace_root.to_path_buf(),
            plan: state.recorded.plan.clone(),
            artifacts,
            frozen_base,
            target,
        })
        .await?;
    let reviewed_bytes = read_artifact(
        recorder,
        product.preview.aggregate_diff_artifact_ref.clone(),
    )
    .await
    .context("failed to read reviewed aggregate diff")?;
    if reviewed_bytes != prepared.aggregate().artifact.content.as_bytes()
        || format!("sha256:{:x}", Sha256::digest(&reviewed_bytes))
            != product.preview.aggregate_diff_digest
        || prepared.aggregate_diff_digest() != product.preview.aggregate_diff_digest
        || prepared.target() != &product.preview.target
    {
        let cleanup = prepared.cleanup().await.err();
        let error = anyhow!("rebuilt integration candidate does not match the reviewed preview");
        return Err(match cleanup {
            Some(cleanup) => error.context(format!(
                "candidate mismatch cleanup also failed: {cleanup:#}"
            )),
            None => error,
        });
    }
    prepared.bind_aggregate_artifact_ref(product.preview.aggregate_diff_artifact_ref.clone())?;
    let verification_policy =
        integration_review_policy(session, &state.recorded.plan.task_id, workspace_root)?;
    if verification_policy.stable_hash()? != product.preview.policy_digest {
        let cleanup = prepared.cleanup().await.err();
        let error = anyhow!("integration verification policy changed after preview");
        return Err(match cleanup {
            Some(cleanup) => error.context(format!(
                "policy-drift candidate cleanup also failed: {cleanup:#}"
            )),
            None => error,
        });
    }
    Ok(PreparedTaskIntegrationReview {
        prepared,
        preview: product.preview,
        verification_policy,
    })
}

fn integration_review_policy(
    session: &Session,
    task_id: &sigil_kernel::TaskId,
    workspace_root: &Path,
) -> Result<VerificationPolicy> {
    let projection = session.verification_state_projection();
    let task_scope = EvidenceScope::Task(task_id.as_str().to_owned());
    let workspace_scope = EvidenceScope::Workspace(stable_workspace_id(workspace_root)?);
    Ok(projection
        .latest_policy(&task_scope)
        .or_else(|| projection.latest_policy(&workspace_scope))
        .map(|entry| entry.policy.clone())
        .unwrap_or_else(|| {
            VerificationPolicy::no_checks_required(
                sigil_kernel::DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
            )
        }))
}

async fn load_review_artifacts(
    entries: &[SessionLogEntry],
    plan: &sigil_kernel::IntegrationPlan,
    recorder: sigil_kernel::MutationEventRecorder,
) -> Result<Vec<IntegrationArtifact>> {
    let mut artifacts = Vec::with_capacity(plan.proposals.len());
    for proposal in &plan.proposals {
        let change_set = entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::ChangeSetProposed(change_set))
                    if change_set.id == proposal.change_set_id =>
                {
                    Some(change_set.clone())
                }
                _ => None,
            })
            .ok_or_else(|| {
                anyhow!(
                    "integration proposal {} is missing its durable changeset",
                    proposal.change_set_id.as_str()
                )
            })?;
        let produced = entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::IsolatedChangeSetProduced(produced))
                    if produced.changeset_id == proposal.change_set_id =>
                {
                    Some(produced)
                }
                _ => None,
            })
            .ok_or_else(|| {
                anyhow!(
                    "integration proposal {} is missing isolated provenance",
                    proposal.change_set_id.as_str()
                )
            })?;
        if produced.base_snapshot_id != proposal.base_snapshot_id
            || produced.integration_facts != proposal.facts
            || produced.artifact_ref.as_deref()
                != Some(proposal.facts.changeset_artifact_ref.as_str())
        {
            bail!(
                "integration proposal {} durable provenance does not match the plan",
                proposal.change_set_id.as_str()
            );
        }
        let bytes = read_artifact(
            recorder.clone(),
            proposal.facts.changeset_artifact_ref.clone(),
        )
        .await
        .with_context(|| {
            format!(
                "failed to read proposal artifact {}",
                proposal.change_set_id.as_str()
            )
        })?;
        if bytes.is_empty() || bytes.len() > MAX_REVIEW_CHANGESET_ARTIFACT_BYTES {
            bail!(
                "integration proposal {} artifact is empty or exceeds the review budget",
                proposal.change_set_id.as_str()
            );
        }
        let content = String::from_utf8(bytes)
            .context("integration proposal artifact is not valid UTF-8 text")?;
        let content_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        artifacts.push(IntegrationArtifact {
            change_set,
            content,
            content_sha256,
        });
    }
    Ok(artifacts)
}

async fn restore_review_frozen_base(
    entries: &[SessionLogEntry],
    workspace_root: &Path,
    state: &sigil_kernel::IntegrationPlanState,
    recorder: sigil_kernel::MutationEventRecorder,
) -> Result<Option<FrozenGitWorktreeBase>> {
    let IntegrationBaseRepresentation::SnapshotWorkspace {
        base_commit,
        overlay_digest,
    } = &state.recorded.plan.base_representation
    else {
        return Ok(None);
    };
    let owned_workspace_ids = state
        .lifecycle_lanes
        .values()
        .filter_map(|lane| {
            lane.prepared
                .as_ref()
                .map(|prepared| prepared.owned_workspace_id.as_str())
        })
        .collect::<BTreeSet<_>>();
    if owned_workspace_ids.len() != state.recorded.plan.lanes.len() {
        bail!("snapshot integration review is missing prepared lane ownership");
    }
    let bindings = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::IsolatedWorkspacePrepared(prepared))
                if owned_workspace_ids.contains(prepared.isolated_workspace_id.as_str()) =>
            {
                Some(prepared)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if bindings.len() != owned_workspace_ids.len() {
        bail!("snapshot integration review is missing durable frozen-base bindings");
    }
    let first = bindings[0];
    let workspace_id = stable_workspace_id(workspace_root)?;
    if bindings.iter().any(|binding| {
        binding.parent_workspace_id != workspace_id
            || binding.base_snapshot_id != state.recorded.plan.base_snapshot_id
            || binding.base_commit.as_deref() != Some(base_commit.as_str())
            || binding.overlay_digest.as_deref() != Some(overlay_digest.as_str())
            || binding.overlay_artifact_ref != first.overlay_artifact_ref
            || binding.overlay_content_artifact_refs != first.overlay_content_artifact_refs
            || binding.overlay_entry_count != first.overlay_entry_count
    }) {
        bail!("snapshot integration review durable frozen-base bindings disagree");
    }
    let overlay_artifact_ref = first
        .overlay_artifact_ref
        .clone()
        .ok_or_else(|| anyhow!("snapshot integration review has no overlay manifest artifact"))?;
    restore_frozen_git_worktree_base(FrozenGitWorktreeBaseRestoreRequest {
        parent_workspace_root: workspace_root.to_path_buf(),
        base_snapshot_id: state.recorded.plan.base_snapshot_id.clone(),
        base_commit: base_commit.clone(),
        overlay_digest: overlay_digest.clone(),
        overlay_artifact_ref,
        overlay_content_artifact_refs: first.overlay_content_artifact_refs.clone(),
        overlay_entry_count: first.overlay_entry_count,
        artifact_recorder: recorder,
    })
    .await
    .map(Some)
}

async fn read_artifact(
    recorder: sigil_kernel::MutationEventRecorder,
    artifact_ref: String,
) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || recorder.read_immutable_content_artifact(&artifact_ref))
        .await
        .context("integration review artifact read task failed")?
}
