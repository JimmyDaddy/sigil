use super::*;
use sigil_kernel::{ApplicationOperationBindingV1, ApplicationOperationTargetV1};

fn decision_binding(
    parent: &Session,
    pending: &sigil_kernel::PublicUserInputRequestV1,
    command_id: &str,
) -> Result<ApplicationOperationBindingV1> {
    ApplicationOperationBindingV1::new(
        parent.session_scope_id().to_owned(),
        sigil_kernel::sha256_hex(command_id.as_bytes()),
        sigil_kernel::sha256_hex(pending.request_hash.as_bytes()),
        ApplicationOperationTargetV1::UserInputDecision {
            request_id: pending.identity.request_id.as_str().to_owned(),
            generation: pending.identity.generation,
            request_hash: pending.request_hash.clone(),
            command_id: command_id.to_owned(),
        },
    )
}

#[tokio::test]
async fn managed_research_operation_queries_original_child_after_parent_terminal() -> Result<()> {
    let (_fixture, mut parent, request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let binding = decision_binding(&parent, &pending, "managed-causal-cancel")?;
    assert!(
        PlanReviewCoordinator::query_managed_research_application_operation(
            &parent,
            &binding,
            provisioner.as_ref(),
        )?
        .expect("research target must not fall through to parent")
        .1
        .is_none()
    );
    let prepared = PlanReviewCoordinator::prepare_managed_research_application_operation(
        &parent,
        &binding,
        provisioner.as_ref(),
    )?
    .expect("actual managed child preparation");
    assert_eq!(prepared.session_scope_id, parent.session_scope_id());
    assert_eq!(
        prepared.domain_session_scope_id(),
        pending.identity.session_scope_id.as_str()
    );
    assert_eq!(prepared.operation_id, binding.operation_id);
    PlanReviewCoordinator::bind_managed_research_application_operation(
        &mut parent,
        &binding,
        provisioner.as_ref(),
    )?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: pending.identity.clone(),
        request_hash: pending.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("managed-causal-cancel")?,
        decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
    };
    let (receipt, continuation, _) =
        PlanReviewCoordinator::accept_plan_review_research_input_with_resources(
            &mut parent,
            command,
            120,
            provisioner.as_ref(),
        )?;
    assert!(!receipt.idempotent_replay);
    assert!(continuation.is_none());
    let _ = parent.clear_application_operation();
    assert_eq!(
        PlanReviewProjection::from_entries(parent.entries())
            .latest_attempt(&request.plan_review_id)
            .expect("terminal parent")
            .status,
        PlanReviewAttemptStatus::Cancelled
    );
    let reopened = parent.application_operation_owner()?.attach_for_control()?;
    let (_, proof) = PlanReviewCoordinator::query_managed_research_application_operation(
        &reopened,
        &binding,
        provisioner.as_ref(),
    )?
    .expect("historical Waiting mirror retains the actual owner");
    let proof = proof.expect("original decision has an actual child commit");
    proof.validate_binding(&prepared)?;
    assert_eq!(
        proof.source_session_scope_id(),
        pending.identity.session_scope_id.as_str()
    );
    assert_eq!(
        PlanReviewCoordinator::committed_managed_research_application_operation(
            &reopened,
            &binding.target,
            &binding.reservation_key_digest,
            provisioner.as_ref(),
        )?,
        Some(Some(prepared.clone()))
    );
    assert_eq!(
        PlanReviewCoordinator::committed_managed_research_application_operation(
            &reopened,
            &binding.target,
            &"f".repeat(64),
            provisioner.as_ref(),
        )?,
        Some(None)
    );
    assert!(
        reopened.entries().iter().all(|entry| !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ApplicationOperationCommittedV1(_))
                | SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(_))
        )),
        "the parent cannot manufacture child completion"
    );
    let mut wrong = binding;
    wrong.domain_session_scope_id = Some("different-child".to_owned());
    assert!(
        PlanReviewCoordinator::query_managed_research_application_operation(
            &reopened,
            &wrong,
            provisioner.as_ref(),
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn managed_research_resume_commits_new_operation_only_at_actual_continuation_start()
-> Result<()> {
    let (fixture, mut parent, _request, pending, provisioner) =
        managed_plan_review_research_waiting_fixture().await?;
    let original = decision_binding(&parent, &pending, "managed-causal-submit")?;
    PlanReviewCoordinator::prepare_managed_research_application_operation(
        &parent,
        &original,
        provisioner.as_ref(),
    )?
    .expect("prepared child decision");
    PlanReviewCoordinator::bind_managed_research_application_operation(
        &mut parent,
        &original,
        provisioner.as_ref(),
    )?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: pending.identity.clone(),
        request_hash: pending.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("managed-causal-submit")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "crates/sigil-kernel".to_owned(),
                },
            }],
        },
    };
    let (_, first_dispatch, _) =
        PlanReviewCoordinator::accept_plan_review_research_input_with_resources(
            &mut parent,
            command.clone(),
            120,
            provisioner.as_ref(),
        )?;
    assert!(
        first_dispatch
            .expect("accepted continuation")
            .application_operation
            .is_none()
    );
    let _ = parent.clear_application_operation();
    let (_, original_proof) = PlanReviewCoordinator::query_managed_research_application_operation(
        &parent,
        &original,
        provisioner.as_ref(),
    )?
    .expect("original research operation");
    let original_proof = original_proof.expect("accepted answer is durably committed");
    // The first supervised dispatch is lost before execution. A new explicit resume has a new
    // K/F but carries the already committed decision; it cannot settle on answer replay alone.
    let resume = ApplicationOperationBindingV1::new(
        parent.session_scope_id().to_owned(),
        sigil_kernel::sha256_hex(b"managed-causal-resume"),
        sigil_kernel::sha256_hex(b"resume-exact-submitted-answer"),
        ApplicationOperationTargetV1::UserInputContinuation {
            original_operation_id: original.operation_id.clone(),
            request_id: pending.identity.request_id.as_str().to_owned(),
            generation: pending.identity.generation,
            request_hash: pending.request_hash.clone(),
        },
    )?;
    PlanReviewCoordinator::prepare_managed_research_application_operation(
        &parent,
        &resume,
        provisioner.as_ref(),
    )?
    .expect("new resume prepared in original child");
    PlanReviewCoordinator::bind_managed_research_application_operation(
        &mut parent,
        &resume,
        provisioner.as_ref(),
    )?;
    let (replayed, dispatch, _) =
        PlanReviewCoordinator::accept_plan_review_research_input_with_resources(
            &mut parent,
            command,
            130,
            provisioner.as_ref(),
        )?;
    assert!(replayed.idempotent_replay);
    let dispatch = dispatch.expect("same supervised research attempt resumes");
    assert_eq!(
        dispatch
            .application_operation
            .as_ref()
            .map(|binding| binding.operation_id.as_str()),
        Some(resume.operation_id.as_str())
    );
    assert!(
        PlanReviewCoordinator::query_managed_research_application_operation(
            &parent,
            &resume,
            provisioner.as_ref(),
        )?
        .expect("resume is a managed target")
        .1
        .is_none()
    );
    let provider = AskingPlanReviewProvider {
        calls: AtomicUsize::new(1),
        ..Default::default()
    };
    let calls = Arc::clone(&provider.request_messages);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut handler = NoopEventHandler;
    let outcome = PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
        &mut parent,
        &dispatch,
        &agent,
        plan_review_test_options(fixture.path()),
        ToolRegistry::new(),
        &mut handler,
        &mut AutoApproveHandler,
        RunCancellationOwner::new().handle(),
        Arc::clone(&provisioner),
    )
    .await?;
    assert!(
        matches!(outcome, PlanReviewRunOutcome::DraftReady { .. }),
        "{outcome:?}"
    );
    PlanReviewCoordinator::close_plan_review_run(
        &mut parent,
        &dispatch,
        &outcome,
        &mut handler,
        140,
    )?;
    let _ = parent.clear_application_operation();
    let reopened = parent.application_operation_owner()?.attach_for_control()?;
    let (resolved, proof) = PlanReviewCoordinator::query_managed_research_application_operation(
        &reopened,
        &resume,
        provisioner.as_ref(),
    )?
    .expect("resume still addresses the original child after parent completion");
    let proof = proof.expect("actual continuation start owns the new commit");
    proof.validate_binding(&resolved)?;
    assert_ne!(proof.event_id(), original_proof.event_id());
    assert!(proof.stream_sequence() > original_proof.stream_sequence());
    let dispatch_count = calls.lock().expect("provider call recorder").len();
    assert!(dispatch_count > 0);
    let (_, original_replay) = PlanReviewCoordinator::query_managed_research_application_operation(
        &reopened,
        &original,
        provisioner.as_ref(),
    )?
    .expect("old K still selects the answer commit");
    assert_eq!(
        original_replay.expect("old committed decision").event_id(),
        original_proof.event_id()
    );
    assert_eq!(
        calls.lock().expect("provider call recorder").len(),
        dispatch_count
    );
    Ok(())
}
