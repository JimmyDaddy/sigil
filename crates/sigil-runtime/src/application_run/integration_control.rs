use super::*;

const MAX_APPLICATION_INTEGRATION_DIFF_BYTES: usize = 4 * 1024 * 1024;

/// Version of the bounded application integration-review projection.
pub const APPLICATION_TASK_INTEGRATION_REVIEW_SCHEMA_VERSION: u16 = 1;

/// Renderer-safe final promotion target kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationIntegrationPromotionTargetKind {
    /// Apply the accepted aggregate to the exact workspace snapshot.
    WorkspaceApply,
    /// Advance one exact user-visible Git ref with compare-and-swap semantics.
    GitRefAdvance,
}

/// Renderer-safe physical lane candidate kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationIntegrationLaneCandidateKind {
    /// Candidate materialized behind a runtime-owned managed ref.
    ManagedRef,
    /// Candidate materialized in an isolated snapshot workspace.
    SnapshotWorkspace,
}

/// Bounded provenance for one reviewed integration lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTaskIntegrationLaneView {
    /// Stable lane identity without a private ref or workspace path.
    pub lane_id: String,
    /// Safe physical candidate classification.
    pub candidate_kind: ApplicationIntegrationLaneCandidateKind,
    /// Number of ordered proposals serialized through this lane.
    pub proposal_count: usize,
    /// Number of lane-scoped verification receipts bound to the candidate.
    pub verification_receipt_count: usize,
}

/// Exact current integration review projected for an application surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTaskIntegrationReviewView {
    /// Projection schema version.
    pub schema_version: u16,
    /// Exact stale-safe action identity that must be echoed on acceptance.
    pub request: sigil_kernel::TaskIntegrationReviewRequest,
    /// Exact aggregate diff selected by the reviewed digest.
    pub aggregate_diff: String,
    /// Content digest for the aggregate diff.
    pub aggregate_diff_digest: String,
    /// Content digest for the complete promotion preview.
    pub preview_digest: String,
    /// Verification policy digest bound to the preview.
    pub policy_digest: String,
    /// Safe target classification. Target refs, object ids, and paths are not exposed.
    pub target_kind: ApplicationIntegrationPromotionTargetKind,
    /// Bounded lane provenance with no runtime-owned ref or workspace identity.
    pub lanes: Vec<ApplicationTaskIntegrationLaneView>,
    /// Child-scoped verification receipts represented by the integration plan.
    pub child_verification_receipt_count: usize,
    /// Lane-scoped verification receipts represented by the promotion preview.
    pub lane_verification_receipt_count: usize,
    /// Conflict causes that forced proposals into serial lanes.
    pub conflict_reasons: Vec<String>,
    /// Number of verification bindings invalidated by promotion.
    pub verification_invalidation_count: usize,
    /// Parent verification always remains pending until the exact preview is accepted.
    pub parent_verification_pending: bool,
}

/// Terminal result of accepting one exact current integration review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTaskIntegrationAcceptanceView {
    /// Exact action identity consumed by the acceptance.
    pub request: sigil_kernel::TaskIntegrationReviewRequest,
    /// Terminal promotion classification.
    pub promotion_status: sigil_kernel::IntegrationPromotionStatus,
    /// Authoritative parent verdict, when promotion reached the parent-check barrier.
    pub parent_verdict: Option<sigil_kernel::VerificationVerdict>,
    /// Whether the exact Task may now continue into synthesis.
    pub can_continue: bool,
    /// Safe cleanup diagnostic for a runtime-owned promotion candidate.
    pub promotion_cleanup_error: Option<String>,
    /// Safe cleanup diagnostic for a retained parent verification target.
    pub parent_cleanup_error: Option<String>,
}

/// Projects the current exact integration review for one bound durable application session.
///
/// The projection verifies the immutable aggregate artifact and its digest before returning any
/// diff bytes. Private worktree paths, managed refs, object ids, artifact refs, and promotion
/// authority are intentionally omitted.
///
/// # Errors
///
/// Returns an error when durable scope, review provenance, or aggregate artifact truth cannot be
/// verified. A session with no current integration review returns `None`.
pub fn application_task_integration_review_view(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<Option<ApplicationTaskIntegrationReviewView>> {
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    let Some(product) = sigil_kernel::task_integration_review_product(&entries) else {
        return Ok(None);
    };
    product
        .request
        .validate_for_preview(&product.preview)
        .context("application integration review identity is stale")?;
    let integration = sigil_kernel::IntegrationProjection::from_entries(&entries);
    let state = integration
        .plans
        .get(&product.preview.plan_id)
        .ok_or_else(|| anyhow!("application integration plan is unavailable"))?;
    if state.inconsistent
        || state.recorded.plan.task_id != product.preview.task_id
        || state.recorded.plan.plan_version != product.preview.plan_version
    {
        bail!("application integration review plan is stale");
    }
    let store = JsonlSessionStore::new(session_path)?;
    let recorder = MutationEventRecorder::new(store);
    let bytes = recorder
        .read_immutable_content_artifact(&product.preview.aggregate_diff_artifact_ref)
        .context("failed to read application integration diff artifact")?;
    if bytes.is_empty() {
        bail!("application integration diff artifact is empty");
    }
    if bytes.len() > MAX_APPLICATION_INTEGRATION_DIFF_BYTES {
        bail!(
            "application integration diff exceeds the {} byte review limit",
            MAX_APPLICATION_INTEGRATION_DIFF_BYTES
        );
    }
    let aggregate_diff_digest = format!("sha256:{}", sigil_kernel::sha256_hex(&bytes));
    if aggregate_diff_digest != product.preview.aggregate_diff_digest {
        bail!("application integration diff digest does not match the current preview");
    }
    let aggregate_diff = String::from_utf8(bytes)
        .map_err(|_| anyhow!("application integration diff is not valid UTF-8 text"))?;
    let plan = &state.recorded.plan;
    let lanes = product
        .preview
        .ordered_lane_candidates
        .iter()
        .map(|lane| {
            let candidate_kind = match lane.candidate {
                sigil_kernel::IntegrationLaneCandidate::ManagedRef { .. } => {
                    ApplicationIntegrationLaneCandidateKind::ManagedRef
                }
                sigil_kernel::IntegrationLaneCandidate::SnapshotWorkspace { .. } => {
                    ApplicationIntegrationLaneCandidateKind::SnapshotWorkspace
                }
            };
            let proposal_count = plan
                .lanes
                .iter()
                .find(|planned| planned.lane_id == lane.lane_id)
                .map_or(0, |planned| planned.proposals.len());
            ApplicationTaskIntegrationLaneView {
                lane_id: lane.lane_id.as_str().to_owned(),
                candidate_kind,
                proposal_count,
                verification_receipt_count: lane.verification_receipt_ids.len(),
            }
        })
        .collect::<Vec<_>>();
    let child_verification_receipt_count = plan
        .proposals
        .iter()
        .map(|proposal| proposal.facts.child_verification_refs.len())
        .sum();
    let lane_verification_receipt_count = product
        .preview
        .ordered_lane_candidates
        .iter()
        .map(|lane| lane.verification_receipt_ids.len())
        .sum();
    let conflict_reasons = plan
        .conflicts
        .iter()
        .flat_map(|conflict| conflict.reasons.iter().copied())
        .map(|reason| reason.as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let target_kind = match product.preview.target {
        sigil_kernel::IntegrationPromotionTarget::WorkspaceApply { .. } => {
            ApplicationIntegrationPromotionTargetKind::WorkspaceApply
        }
        sigil_kernel::IntegrationPromotionTarget::GitRefAdvance { .. } => {
            ApplicationIntegrationPromotionTargetKind::GitRefAdvance
        }
    };
    Ok(Some(ApplicationTaskIntegrationReviewView {
        schema_version: APPLICATION_TASK_INTEGRATION_REVIEW_SCHEMA_VERSION,
        request: product.request,
        aggregate_diff,
        aggregate_diff_digest,
        preview_digest: product.preview.preview_digest,
        policy_digest: product.preview.policy_digest,
        target_kind,
        lanes,
        child_verification_receipt_count,
        lane_verification_receipt_count,
        conflict_reasons,
        verification_invalidation_count: product.preview.verification_invalidation.len(),
        parent_verification_pending: true,
    }))
}

/// Accepts one exact current integration review under the shared application session lease.
///
/// The accepted request is revalidated against freshly loaded durable truth before promotion.
/// Promotion and authoritative parent checks reuse the same execution backend and append-only
/// event path as the TUI. The caller must issue a separate exact Task continuation after
/// `can_continue` becomes true.
///
/// # Errors
///
/// Returns an error when another foreground operation owns the session, the request is stale,
/// workspace or policy truth drifted, promotion fails, or parent verification cannot be recorded.
pub async fn accept_application_task_integration_review(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    services: &ApplicationRunServices,
    request: &sigil_kernel::TaskIntegrationReviewRequest,
) -> Result<ApplicationTaskIntegrationAcceptanceView> {
    accept_application_task_integration_review_with_attachment(
        config_path,
        launch_cwd,
        session_path,
        expected_session_scope_id,
        services,
        request,
        None,
    )
    .await
}

/// Accepts integration review while reusing a controller-owned session attachment.
pub async fn accept_application_task_integration_review_with_attachment(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    services: &ApplicationRunServices,
    request: &sigil_kernel::TaskIntegrationReviewRequest,
    session_attachment: Option<
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    >,
) -> Result<ApplicationTaskIntegrationAcceptanceView> {
    let config_path = config_path.to_owned();
    let launch_cwd = launch_cwd.to_owned();
    let session_path = session_path.to_owned();
    let expected_session_scope_id = expected_session_scope_id.to_owned();
    let session_leases = Arc::clone(&services.session_leases);
    let request = request.clone();
    let preparation = tokio::task::spawn_blocking(move || {
        let root_config = RootConfig::load(&config_path)?;
        let workspace_root =
            resolve_workspace_root(&config_path, &launch_cwd, &root_config.workspace.root);
        let store = JsonlSessionStore::new(&session_path)?;
        let session_lease =
            session_leases.acquire_with_attachment(store.path(), session_attachment)?;
        let session = Session::load_from_store(
            root_config.agent.runtime_provider.clone(),
            root_config.agent.model.clone(),
            store,
        )?;
        if session.session_scope_id() != expected_session_scope_id {
            bail!("durable session identity changed before integration acceptance");
        }
        let execution_backend = crate::build_configured_execution_backend(&root_config)?;
        let secret_redactor = crate::secret_redactor_for_root_config(&root_config);
        Ok::<_, anyhow::Error>((
            session,
            session_lease,
            workspace_root,
            execution_backend,
            secret_redactor,
            request,
        ))
    })
    .await
    .map_err(|_| anyhow!("integration acceptance preparation worker failed"))??;
    let (mut session, _session_lease, workspace_root, execution_backend, secret_redactor, request) =
        preparation;
    let mut handler = NoopEventHandler;
    let output = crate::integration_lanes::accept_task_integration_review(
        &mut session,
        &mut handler,
        execution_backend,
        &secret_redactor,
        &workspace_root,
        &request,
    )
    .await?;
    let promotion_status = output.promotion.record.status;
    let promotion_cleanup_error = output
        .promotion
        .cleanup_error
        .map(|error| safe_persistence_text(&error));
    let parent_verdict = output
        .parent_verification
        .as_ref()
        .map(|parent| parent.record.verdict);
    let parent_cleanup_error = output
        .parent_verification
        .and_then(|parent| parent.cleanup_error)
        .map(|error| safe_persistence_text(&error));
    let can_continue = promotion_status == sigil_kernel::IntegrationPromotionStatus::Promoted
        && matches!(
            parent_verdict,
            Some(
                sigil_kernel::VerificationVerdict::Passed
                    | sigil_kernel::VerificationVerdict::NotApplicable
            )
        );
    Ok(ApplicationTaskIntegrationAcceptanceView {
        request,
        promotion_status,
        parent_verdict,
        can_continue,
        promotion_cleanup_error,
        parent_cleanup_error,
    })
}
