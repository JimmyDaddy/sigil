//! Research operation capabilities never outlive the RA bundle that authenticates the child.

use super::*;
use sigil_kernel::session::ApplicationOperationCommitProofV1;
use sigil_kernel::{ApplicationOperationBindingV1, ApplicationOperationTargetV1};

fn research_target(
    parent: &Session,
    target: &ApplicationOperationTargetV1,
) -> Result<Option<(PlanReviewRunRequest, sigil_kernel::PublicUserInputRequestV1)>> {
    let (request_id, generation, request_hash) = match target {
        ApplicationOperationTargetV1::UserInputDecision {
            request_id,
            generation,
            request_hash,
            ..
        }
        | ApplicationOperationTargetV1::UserInputContinuation {
            request_id,
            generation,
            request_hash,
            ..
        } => (request_id, generation, request_hash),
        _ => return Ok(None),
    };
    let projection = PlanReviewProjection::from_entries(parent.entries());
    if projection.has_conflicts() {
        bail!("research operation parent contains conflicting attempts");
    }
    let request_id = sigil_kernel::UserInputRequestId::new(request_id.clone())?;
    let Some(attempt) =
        projection.attempt_for_pending_user_input_key(&request_id, *generation, request_hash)
    else {
        return Ok(None);
    };
    let pending = attempt
        .pending_user_input
        .as_deref()
        .context("research operation attempt lost its input mirror")?;
    if !matches!(&pending.source, sigil_kernel::UserInputSourceV1::PlanReviewResearch {
        plan_review_id, attempt_id,
    } if plan_review_id == &attempt.plan_review_id && attempt_id == &attempt.attempt_id)
    {
        bail!("research operation source does not match its parent attempt");
    }
    let request = plan_review_request_from_attempt(parent, attempt)?;
    validate_managed_plan_review_request_binding(&request)?;
    if pending.identity.root_logical_run_id.as_str() != request.child_logical_run_id() {
        bail!("research operation changed its logical child run");
    }
    Ok(Some((request, pending.clone())))
}

impl PlanReviewCoordinator {
    /// Identifies a research target from the parent's durable mirror before selecting its RA
    /// provisioner. Missing managed composition cannot turn it into a parent-owned operation.
    ///
    /// # Errors
    /// Returns an error when the durable research mirror is conflicting or malformed.
    pub fn is_managed_research_application_target(
        parent: &Session,
        target: &ApplicationOperationTargetV1,
    ) -> Result<bool> {
        Ok(research_target(parent, target)?.is_some())
    }

    /// Prepares an operation in the actual managed child. `None` means this target is not a
    /// research request; an unavailable or conflicting research child is always an error.
    ///
    /// # Errors
    /// Returns an error when RA admission, exact owner validation, or durable preparation fails.
    pub fn prepare_managed_research_application_operation(
        parent: &Session,
        binding: &ApplicationOperationBindingV1,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<Option<ApplicationOperationBindingV1>> {
        let Some((request, pending)) = research_target(parent, &binding.target)? else {
            return Ok(None);
        };
        let bundle = provisioner.mutate_research_session(&request)?;
        let result = bundle.with_child_session(
            parent,
            pending.identity.session_scope_id.as_str(),
            |child| {
                let owner = parent
                    .application_operation_owner()?
                    .delegate_existing_child(child, &binding.target)?;
                owner.prepare(binding)?;
                Ok(owner.resolve_operation(binding)?.1)
            },
        );
        combine_child_resource_settlement(result, bundle.finish()).map(Some)
    }

    /// Freezes the prepared child binding on a parent runtime attachment without exporting the
    /// child's writer or physical path. The real child consumes it later inside its RA bundle.
    ///
    /// # Errors
    /// Returns an error when the managed child cannot prepare or validate the original binding.
    pub fn bind_managed_research_application_operation(
        parent: &mut Session,
        binding: &ApplicationOperationBindingV1,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<bool> {
        let Some((request, pending)) = research_target(parent, &binding.target)? else {
            return Ok(false);
        };
        let snapshot = parent.application_operation_owner()?.attach_for_control()?;
        let bundle = provisioner.mutate_research_session(&request)?;
        let result = bundle.with_child_session(
            &snapshot,
            pending.identity.session_scope_id.as_str(),
            |child| {
                let owner = snapshot
                    .application_operation_owner()?
                    .delegate_existing_child(child, &binding.target)?;
                owner.prepare(binding)?;
                parent.bind_application_operation_from_owner(binding.clone(), &owner)
            },
        );
        combine_child_resource_settlement(result, bundle.finish())?;
        Ok(true)
    }

    /// Reconciles the original K/F using recovery-only RA admission. The outer `None` means
    /// non-research; a managed operation without a commit returns `Some((binding, None))` and
    /// must never fall through to another domain owner.
    ///
    /// # Errors
    /// Returns an error when recovery admission or the parent/child durable proof is invalid.
    pub fn query_managed_research_application_operation(
        parent: &Session,
        binding: &ApplicationOperationBindingV1,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<
        Option<(
            ApplicationOperationBindingV1,
            Option<ApplicationOperationCommitProofV1>,
        )>,
    > {
        let Some((request, _)) = research_target(parent, &binding.target)? else {
            return Ok(None);
        };
        let bundle = provisioner.recover_research_session(&request)?;
        let result = (|| {
            let records = bundle.records()?;
            let resolved = parent
                .application_operation_owner()?
                .bind_child_records(&records, binding)?;
            let proof = sigil_kernel::session::reconcile_application_operation_records(
                &records, &resolved,
            )?;
            Ok((resolved, proof))
        })();
        combine_child_resource_settlement(result, bundle.finish()).map(Some)
    }

    /// Finds a committed original operation by its exact application key inside the managed
    /// child. Outer `None` means non-research; inner `None` means the research child has no
    /// committed operation for that key. No read or write capability escapes the RA admission.
    ///
    /// # Errors
    /// Returns an error for unavailable resources, conflicting keys, or invalid durable proof.
    pub fn committed_managed_research_application_operation(
        parent: &Session,
        target: &ApplicationOperationTargetV1,
        key_digest: &str,
        provisioner: &dyn PlanReviewChildResourceProvisionerV1,
    ) -> Result<Option<Option<ApplicationOperationBindingV1>>> {
        let Some((request, _)) = research_target(parent, target)? else {
            return Ok(None);
        };
        let bundle = provisioner.recover_research_session(&request)?;
        let result = (|| {
            let records = bundle.records()?;
            let mut found = None;
            for record in &records {
                if let Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(
                    binding,
                ))) = record.session_log_entry()?
                    && binding.reservation_key_digest == key_digest
                {
                    if &binding.target != target
                        || found.as_ref().is_some_and(|prior| prior != &binding)
                    {
                        bail!("research operation key has conflicting original context");
                    }
                    found = Some(binding);
                }
            }
            let Some(binding) = found else {
                return Ok(None);
            };
            let resolved = parent
                .application_operation_owner()?
                .bind_child_records(&records, &binding)?;
            Ok(
                sigil_kernel::session::reconcile_application_operation_records(
                    &records, &resolved,
                )?
                .map(|_| resolved),
            )
        })();
        combine_child_resource_settlement(result, bundle.finish()).map(Some)
    }
}
