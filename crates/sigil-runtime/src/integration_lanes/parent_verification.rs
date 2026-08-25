use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Result, anyhow, bail};
use sigil_kernel::verification::VerificationExecutionPortV1;
use sigil_kernel::{
    ControlEntry, EventHandler, EvidenceScope, IntegrationPlanId, IntegrationPromotionAttemptId,
    ReadinessInput, ReceiptStatus, RunEvent, RunStatus, Session, TaskParentVerificationRecorded,
    TrustedCheckSpec, VerificationCheckRunRequest, VerificationPolicy, VerificationRecordedEntry,
    VerificationVerdict, WorkspaceTrust, build_workspace_snapshot, evaluate_readiness,
    run_verification_check, stable_workspace_id,
};

use super::{PromotedVerificationTarget, unix_time_ms};

/// Exact accepted-policy input for checks on one authoritative promoted target.
pub struct ParentVerificationRunRequest {
    pub attempt_id: IntegrationPromotionAttemptId,
    pub plan_id: IntegrationPlanId,
    pub preview_digest: String,
    pub promoted_snapshot_id: String,
    pub policy_digest: String,
    pub policy: VerificationPolicy,
    pub trusted_checks: Vec<TrustedCheckSpec>,
    pub workspace_trust: WorkspaceTrust,
    pub workspace_trust_snapshot_id: String,
    pub workspace_trust_approval_event_id: Option<String>,
    pub workspace_trust_sandbox_decision_id: Option<String>,
    pub target: PromotedVerificationTarget,
}

/// Terminal parent-check record and cleanup diagnostics for the promoted target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentVerificationRunOutput {
    pub record: TaskParentVerificationRecorded,
    pub cleanup_error: Option<String>,
}

/// Runs the accepted verification policy on the exact target retained by promotion.
///
/// Every check uses the RFC-0003 execution path and produces an ordinary durable verification
/// receipt before the attempt-level parent verdict is appended. The promoted target is snapshotted
/// before and after the checks; drift or a scope-writing check can never produce `Passed`.
///
/// # Errors
///
/// Returns an error before check execution for a policy/preview mismatch, missing or substituted
/// trusted checks, or durable append failure. Check execution failures become a terminal
/// `Inconclusive` parent record so callers can recover without replaying an uncertain command.
pub async fn run_authoritative_parent_verification<H>(
    session: &mut Session,
    handler: &mut H,
    verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
    request: ParentVerificationRunRequest,
) -> Result<ParentVerificationRunOutput>
where
    H: EventHandler + Send,
{
    if let Err(error) = validate_request(&request) {
        return cleanup_rejected_parent_verification(request.target, error).await;
    }
    let policy_digest = request.policy_digest.clone();
    let workspace_root = request.target.workspace_root().to_path_buf();
    let evidence_scope = EvidenceScope::Task(format!(
        "parent-verification:{}:{}",
        request.plan_id.as_str(),
        request.attempt_id.as_str()
    ));
    let initial_snapshot = match capture_policy_snapshot(&workspace_root, &request.policy).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return cleanup_rejected_parent_verification(request.target, error).await;
        }
    };
    let Some(initial_snapshot_id) = initial_snapshot.workspace_snapshot_id.as_deref() else {
        return cleanup_rejected_parent_verification(
            request.target,
            anyhow!("authoritative parent verification snapshot is incomplete"),
        )
        .await;
    };
    let mut receipts = Vec::new();
    let mut execution_error = None;
    let initial_matches = initial_snapshot_id == request.promoted_snapshot_id;

    if initial_matches {
        let trusted_checks = request
            .trusted_checks
            .iter()
            .map(|check| (check.check_spec.check_spec_id.as_str(), check.clone()))
            .collect::<BTreeMap<_, _>>();
        for required in &request.policy.required_checks {
            let trusted_check = trusted_checks
                .get(required.check_spec_id.as_str())
                .expect("validated parent check must exist")
                .clone();
            let verification = run_verification_check(
                session,
                verification_execution_port.as_ref(),
                VerificationCheckRunRequest {
                    workspace_root: workspace_root.clone(),
                    scope: evidence_scope.clone(),
                    trusted_check,
                    policy: request.policy.clone(),
                    policy_hash: Some(policy_digest.clone()),
                    workspace_trust: request.workspace_trust,
                    workspace_trust_snapshot_id: request.workspace_trust_snapshot_id.clone(),
                    workspace_trust_approval_event_id: request
                        .workspace_trust_approval_event_id
                        .clone(),
                    workspace_trust_sandbox_decision_id: request
                        .workspace_trust_sandbox_decision_id
                        .clone(),
                },
            )
            .await;
            match verification {
                Ok(verification) => {
                    if let Err(error) = append_verification_record(session, handler, &verification)
                    {
                        return cleanup_rejected_parent_verification(request.target, error).await;
                    }
                    let receipt = verification.receipt;
                    let stop = receipt.mutates_verification_scope
                        || receipt.binding.workspace_snapshot_id != request.promoted_snapshot_id;
                    receipts.push(receipt);
                    if stop {
                        break;
                    }
                }
                Err(error) => {
                    execution_error = Some(format!("{error:#}"));
                    break;
                }
            }
        }
    }

    let final_snapshot = match capture_policy_snapshot(&workspace_root, &request.policy).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return cleanup_rejected_parent_verification(request.target, error).await;
        }
    };
    let final_snapshot_id = final_snapshot.workspace_snapshot_id.as_deref();
    let snapshot_stale = !initial_matches
        || final_snapshot_id != Some(request.promoted_snapshot_id.as_str())
        || receipts.iter().any(|receipt| {
            receipt.binding.workspace_snapshot_id != request.promoted_snapshot_id
                || receipt.binding.verification_scope_hash
                    != request.policy.verification_scope.scope_hash
                || receipt.mutates_verification_scope
        });
    let mut readiness = ReadinessInput::new_run(RunStatus::Completed, request.policy.clone());
    readiness.workspace_trust = request.workspace_trust;
    readiness.workspace_trust_approval_event_id = request.workspace_trust_approval_event_id.clone();
    readiness.workspace_trust_sandbox_decision_id =
        request.workspace_trust_sandbox_decision_id.clone();
    readiness.current_workspace_snapshot_id = final_snapshot_id.map(str::to_owned);
    readiness.workspace_knowledge = final_snapshot.workspace_knowledge;
    readiness.verification_receipts = receipts.clone();
    let evaluated = evaluate_readiness(&readiness);
    let verdict = if snapshot_stale {
        VerificationVerdict::Stale
    } else if execution_error.is_some() {
        VerificationVerdict::Inconclusive
    } else {
        evaluated.verification_verdict
    };
    let cleanup_error = request
        .target
        .cleanup()
        .await
        .err()
        .map(|error| format!("{error:#}"));
    let verdict = if cleanup_error.is_some() && verdict == VerificationVerdict::Passed {
        VerificationVerdict::Inconclusive
    } else {
        verdict
    };
    let retained_receipts = receipts
        .into_iter()
        .filter(|receipt| {
            receipt.binding.workspace_snapshot_id == request.promoted_snapshot_id
                && receipt.binding.verification_scope_hash
                    == request.policy.verification_scope.scope_hash
                && (verdict != VerificationVerdict::Passed
                    || (receipt.check_status == ReceiptStatus::Succeeded
                        && receipt.receipt.status == ReceiptStatus::Succeeded
                        && !receipt.mutates_verification_scope))
        })
        .collect::<Vec<_>>();
    let reason = parent_verification_reason(
        verdict,
        initial_matches,
        final_snapshot_id,
        &request.promoted_snapshot_id,
        execution_error,
        cleanup_error.as_deref(),
    );
    let record = TaskParentVerificationRecorded {
        attempt_id: request.attempt_id,
        plan_id: request.plan_id,
        preview_digest: request.preview_digest,
        promoted_snapshot_id: request.promoted_snapshot_id,
        policy_digest,
        verdict,
        receipts: retained_receipts,
        reason,
        recorded_at_unix_ms: unix_time_ms(),
    };
    record.validate()?;
    session.append_control(ControlEntry::TaskParentVerificationRecorded(record.clone()))?;
    handler.handle(RunEvent::Control(
        ControlEntry::TaskParentVerificationRecorded(record.clone()),
    ))?;
    Ok(ParentVerificationRunOutput {
        record,
        cleanup_error,
    })
}

async fn cleanup_rejected_parent_verification(
    target: PromotedVerificationTarget,
    error: anyhow::Error,
) -> Result<ParentVerificationRunOutput> {
    match target.cleanup().await {
        Ok(()) => Err(error),
        Err(cleanup) => Err(error.context(format!(
            "rejected parent verification target cleanup also failed: {cleanup:#}"
        ))),
    }
}

fn validate_request(request: &ParentVerificationRunRequest) -> Result<()> {
    if request.preview_digest.trim().is_empty()
        || request.promoted_snapshot_id.trim().is_empty()
        || request.policy_digest.trim().is_empty()
        || request.workspace_trust_snapshot_id.trim().is_empty()
    {
        bail!("authoritative parent verification binding is incomplete");
    }
    if request.policy.stable_hash()? != request.policy_digest {
        bail!("authoritative parent verification policy does not match the promotion preview");
    }
    let mut trusted_by_id = BTreeMap::new();
    for trusted in &request.trusted_checks {
        let check = &trusted.check_spec;
        if check.verification_scope_hash != request.policy.verification_scope.scope_hash {
            bail!(
                "trusted parent check {} belongs to another verification scope",
                check.check_spec_id
            );
        }
        if trusted_by_id
            .insert(check.check_spec_id.as_str(), check.check_spec_hash.as_str())
            .is_some()
        {
            bail!("trusted parent verification checks contain a duplicate id");
        }
    }
    if trusted_by_id.len() != request.policy.required_checks.len()
        || request.policy.required_checks.iter().any(|required| {
            trusted_by_id.get(required.check_spec_id.as_str())
                != Some(&required.check_spec_hash.as_str())
        })
    {
        bail!("trusted parent verification checks do not match the accepted policy");
    }
    Ok(())
}

async fn capture_policy_snapshot(
    workspace_root: &std::path::Path,
    policy: &VerificationPolicy,
) -> Result<sigil_kernel::WorkspaceSnapshotBuild> {
    let workspace_root = workspace_root.to_path_buf();
    let scope = policy.verification_scope.clone();
    tokio::task::spawn_blocking(move || {
        let workspace_id = stable_workspace_id(&workspace_root)?;
        build_workspace_snapshot(&workspace_root, workspace_id, &scope, 0)
    })
    .await
    .map_err(|error| anyhow!("authoritative parent snapshot task failed: {error}"))?
}

fn append_verification_record<H>(
    session: &mut Session,
    handler: &mut H,
    verification: &VerificationRecordedEntry,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let control = ControlEntry::VerificationRecorded(verification.clone());
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

fn parent_verification_reason(
    verdict: VerificationVerdict,
    initial_matches: bool,
    final_snapshot_id: Option<&str>,
    promoted_snapshot_id: &str,
    execution_error: Option<String>,
    cleanup_error: Option<&str>,
) -> Option<String> {
    if !initial_matches {
        return Some("authoritative target snapshot was stale before parent checks".to_owned());
    }
    if final_snapshot_id != Some(promoted_snapshot_id) {
        return Some("authoritative target snapshot changed during parent checks".to_owned());
    }
    if let Some(error) = execution_error {
        return Some(format!(
            "parent verification check could not complete: {error}"
        ));
    }
    if let Some(error) = cleanup_error {
        return Some(format!(
            "parent verification completed but owned target cleanup failed: {error}"
        ));
    }
    if verdict == VerificationVerdict::NotApplicable {
        return Some("accepted parent verification policy requires no checks".to_owned());
    }
    (verdict != VerificationVerdict::Passed)
        .then(|| format!("accepted parent verification policy evaluated to {verdict:?}"))
}
