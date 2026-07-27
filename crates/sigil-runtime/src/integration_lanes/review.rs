use std::{collections::BTreeSet, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, EventHandler, EvidenceScope, ExecutionBackend, IntegrationBaseRepresentation,
    IntegrationProjection, IntegrationPromotionAttemptId, IntentExecutionOriginV1, RunEvent,
    SecretRedactor, Session, SessionLogEntry, TaskIntegrationReviewRequest, TaskPromotionAuthority,
    TaskPromotionPreview, TrustedCheckSpec, VerificationPolicy, WorkspaceTrust,
    materialize_intent_layer, stable_event_uuid, stable_workspace_id,
};

use super::{
    GitIntegrationPromotionOutput, GitIntegrationPromotionPreparationRequest,
    GitIntegrationPromotionRunRequest, IntegrationArtifact, IntegrationPromotionPreparationTarget,
    IntegrationPromotionRuntimeEvent, IntegrationPromotionRuntimeEventRequest,
    ParentVerificationRunOutput, ParentVerificationRunRequest, PreparedGitIntegrationPromotion,
    prepare_git_integration_promotion, run_authoritative_parent_verification,
    run_git_integration_promotion_with_events, unix_time_ms,
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

/// Terminal promotion plus authoritative parent verification for one accepted exact review.
#[derive(Debug)]
pub struct TaskIntegrationAcceptanceOutput {
    pub promotion: GitIntegrationPromotionOutput,
    pub parent_verification: Option<ParentVerificationRunOutput>,
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

/// Consumes one exact user integration review and runs the final promotion/parent-check barrier.
///
/// Authority and terminal promotion facts are durably appended and acknowledged before the
/// runtime advances to the next effect. Trusted checks and workspace trust are projected before
/// the physical promotion so an incomplete verification context cannot mutate the parent.
///
/// # Errors
///
/// Returns an error before promotion for stale review, policy/provenance drift or incomplete
/// trusted checks, and after durable recovery facts for acknowledged runtime failures.
pub async fn accept_task_integration_review<H>(
    session: &mut Session,
    handler: &mut H,
    execution_backend: Arc<dyn ExecutionBackend>,
    secret_redactor: &SecretRedactor,
    workspace_root: &Path,
    request: &TaskIntegrationReviewRequest,
) -> Result<TaskIntegrationAcceptanceOutput>
where
    H: EventHandler + Send,
{
    let reviewed = prepare_task_integration_review(session, workspace_root, request).await?;
    let parent_context = parent_verification_context(
        session,
        &reviewed.preview.task_id,
        workspace_root,
        &reviewed.verification_policy,
    )?;
    let now = unix_time_ms();
    let attempt_id = IntegrationPromotionAttemptId::new(format!(
        "promotion-{}",
        stable_event_uuid("sigil-task-integration-promotion", &request.request_id)
    ))?;
    let authority = TaskPromotionAuthority::from_user_integration_review(
        &reviewed.preview,
        request.request_id.clone(),
        now.saturating_add(5 * 60 * 1_000),
        format!(
            "nonce-{}",
            stable_event_uuid("sigil-task-integration-authority", &request.request_id)
        ),
    )?;
    let mutation_recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| anyhow!("task integration acceptance requires durable mutation storage"))?;
    let preview = reviewed.preview;
    let verification_policy = reviewed.verification_policy;
    let (event_sender, mut event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<IntegrationPromotionRuntimeEventRequest>();
    let mut promotion = Box::pin(run_git_integration_promotion_with_events(
        GitIntegrationPromotionRunRequest {
            prepared: reviewed.prepared,
            attempt_id: attempt_id.clone(),
            preview: preview.clone(),
            authority,
            verification_policy: verification_policy.clone(),
            mutation_recorder,
        },
        event_sender,
    ));
    let mut append_error: Option<String> = None;
    let promotion = loop {
        tokio::select! {
            event = event_receiver.recv() => {
                let Some(event) = event else {
                    break promotion.await;
                };
                let acknowledgement = if let Some(error) = append_error.as_ref() {
                    Err(error.clone())
                } else {
                    match append_promotion_runtime_event(session, handler, event.event().clone()) {
                        Ok(()) => Ok(()),
                        Err(error) => {
                            let message = format!("{error:#}");
                            append_error = Some(message.clone());
                            Err(message)
                        }
                    }
                };
                event.acknowledge(acknowledgement);
            }
            result = &mut promotion => break result,
        }
    };
    let mut promotion = match (promotion, append_error) {
        (Ok(output), None) => output,
        (Err(error), _) => return Err(error),
        (Ok(_), Some(error)) => bail!("failed to persist integration promotion event: {error}"),
    };
    if promotion.record.status != sigil_kernel::IntegrationPromotionStatus::Promoted {
        return Ok(TaskIntegrationAcceptanceOutput {
            promotion,
            parent_verification: None,
        });
    }
    let promoted_snapshot_id = promotion
        .authoritative_snapshot_id
        .clone()
        .ok_or_else(|| anyhow!("promoted integration target has no authoritative snapshot"))?;
    let target = promotion
        .verification_target
        .take()
        .ok_or_else(|| anyhow!("promoted integration target was not retained for verification"))?;
    if let Err(error) = materialize_promoted_intent_layers(
        session,
        workspace_root,
        &promotion.record.plan_id,
        secret_redactor,
        handler,
    ) {
        let _ = handler.handle(RunEvent::Notice(format!(
            "Intent Stack remained read-only after promotion: {error:#}"
        )));
    }
    let target_root = target.workspace_root().to_path_buf();
    let parent_verification = run_authoritative_parent_verification(
        session,
        handler,
        execution_backend,
        ParentVerificationRunRequest {
            attempt_id,
            plan_id: preview.plan_id,
            preview_digest: preview.preview_digest,
            promoted_snapshot_id,
            policy_digest: preview.policy_digest,
            policy: verification_policy,
            trusted_checks: parent_context.trusted_checks,
            workspace_trust: parent_context.workspace_trust,
            workspace_trust_snapshot_id: parent_context.workspace_trust_snapshot_id,
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            target,
        },
    )
    .await
    .with_context(|| {
        format!(
            "authoritative parent verification failed for {}",
            target_root.display()
        )
    })?;
    Ok(TaskIntegrationAcceptanceOutput {
        promotion,
        parent_verification: Some(parent_verification),
    })
}

fn materialize_promoted_intent_layers<H>(
    session: &Session,
    workspace_root: &Path,
    plan_id: &sigil_kernel::IntegrationPlanId,
    secret_redactor: &SecretRedactor,
    handler: &mut H,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let integration = IntegrationProjection::from_entries(session.entries());
    let plan = &integration
        .plans
        .get(plan_id)
        .ok_or_else(|| anyhow!("promoted integration plan is unavailable"))?
        .recorded
        .plan;
    let task_projection = session.task_state_projection();
    let task_plan = task_projection
        .tasks
        .get(&plan.task_id)
        .and_then(|task| task.plans.get(&plan.plan_version))
        .filter(|task_plan| task_plan.status == sigil_kernel::TaskPlanStatus::Accepted)
        .ok_or_else(|| anyhow!("promoted integration has no accepted TaskPlan"))?;
    let lineage = session.intent_lineage_projection()?;
    for proposal in &plan.proposals {
        let step = task_plan
            .steps
            .iter()
            .find(|step| step.step_id == proposal.step_id)
            .ok_or_else(|| anyhow!("promoted integration step is absent from its TaskPlan"))?;
        let [intent_ref] = step.intent_refs.as_slice() else {
            if step.intent_refs.is_empty() {
                continue;
            }
            bail!(
                "promoted integration step {} has ambiguous Intent refs",
                step.step_id.as_str()
            );
        };
        let execution = lineage
            .execution_order
            .iter()
            .rev()
            .filter_map(|execution_id| lineage.execution(execution_id))
            .find(|execution| {
                execution.binding.intent_ref == *intent_ref
                    && matches!(
                        &execution.binding.origin,
                        IntentExecutionOriginV1::Task {
                            task_id,
                            task_plan_version,
                            step_id,
                            ..
                        } if task_id == plan.task_id.as_str()
                            && *task_plan_version == plan.plan_version
                            && step_id == proposal.step_id.as_str()
                    )
                    && execution.changeset_ids.contains(&proposal.change_set_id)
            })
            .ok_or_else(|| {
                anyhow!(
                    "promoted integration proposal {} has no exact Intent execution lineage",
                    proposal.change_set_id.as_str()
                )
            })?;
        let outcome = materialize_intent_layer(
            session,
            workspace_root,
            &execution.binding.execution_id,
            secret_redactor,
        )?;
        if let Some(reason) = outcome.read_only_reason {
            let _ = handler.handle(RunEvent::Notice(format!(
                "Intent layer for step {} is read-only: {reason:?}",
                step.step_id.as_str()
            )));
        }
    }
    Ok(())
}

struct ParentVerificationContext {
    trusted_checks: Vec<TrustedCheckSpec>,
    workspace_trust: WorkspaceTrust,
    workspace_trust_snapshot_id: String,
}

fn parent_verification_context(
    session: &Session,
    task_id: &sigil_kernel::TaskId,
    workspace_root: &Path,
    policy: &VerificationPolicy,
) -> Result<ParentVerificationContext> {
    let projection = session.verification_state_projection();
    let workspace_id = stable_workspace_id(workspace_root)?;
    let task_scope = EvidenceScope::Task(task_id.as_str().to_owned());
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    let projected = projection.check_specs_for_scopes(&[task_scope, workspace_scope]);
    let mut trusted_checks = Vec::with_capacity(policy.required_checks.len());
    for required in &policy.required_checks {
        let trusted = projected
            .iter()
            .find(|entry| {
                entry.trusted_check.check_spec.check_spec_id == required.check_spec_id
                    && entry.trusted_check.check_spec.check_spec_hash == required.check_spec_hash
            })
            .ok_or_else(|| {
                anyhow!(
                    "accepted parent verification check {} is unavailable or changed",
                    required.check_spec_id
                )
            })?;
        trusted_checks.push(trusted.trusted_check.clone());
    }
    let trust = projection.workspace_trust.get(&workspace_id);
    Ok(ParentVerificationContext {
        trusted_checks,
        workspace_trust: trust.map_or(WorkspaceTrust::Unknown, |entry| entry.trust),
        workspace_trust_snapshot_id: trust
            .map(|entry| entry.workspace_trust_snapshot_id.clone())
            .unwrap_or_else(|| format!("workspace-trust:unknown:{workspace_id}")),
    })
}

fn append_promotion_runtime_event<H>(
    session: &mut Session,
    handler: &mut H,
    event: IntegrationPromotionRuntimeEvent,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let control = match event {
        IntegrationPromotionRuntimeEvent::AuthorityConsumed(entry) => {
            ControlEntry::TaskPromotionAuthorityConsumed(entry)
        }
        IntegrationPromotionRuntimeEvent::PromotionRecorded(entry) => {
            ControlEntry::IntegrationPromotionRecorded(entry)
        }
    };
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
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
