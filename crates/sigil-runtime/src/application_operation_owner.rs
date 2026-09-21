//! Application K/F translation and reconciliation against the session domain owner.

use sigil_application::{
    ApplicationCommand, ApplicationCommandRequest, ApplicationDomainCommitRef,
    ApplicationDomainReceipt, ApplicationError, ApplicationFrontier, ApplicationQueueAction,
    ApplicationQueueItemKind, ApplicationQueueTarget, PlanTaskCommand, UserInputCommand,
};
use sigil_kernel::{
    ApplicationOperationBindingV1, ApplicationOperationTargetV1, ConversationInputKind,
    ConversationInputTarget, SessionRecordReadHandle,
};

pub fn application_operation_binding(
    request: &ApplicationCommandRequest,
) -> Result<Option<ApplicationOperationBindingV1>, ApplicationError> {
    use ApplicationOperationTargetV1 as Target;
    request.validate()?;
    let target = match &request.envelope.command {
        ApplicationCommand::Conversation(sigil_application::ConversationCommand::Queue {
            action,
            ..
        }) => match action {
            ApplicationQueueAction::Enqueue {
                target,
                prompt,
                kind,
                ..
            } => Target::QueueEnqueue {
                prompt_hash: queue_prompt_hash(prompt.as_str()),
                target: queue_target(target)?,
                input_kind: match kind {
                    ApplicationQueueItemKind::Chat => ConversationInputKind::Chat,
                    ApplicationQueueItemKind::PlanPrompt => ConversationInputKind::PlanPrompt,
                    ApplicationQueueItemKind::AgentMention => ConversationInputKind::AgentMention,
                    ApplicationQueueItemKind::AgentMessage => ConversationInputKind::AgentMessage,
                    ApplicationQueueItemKind::TaskGuidance => ConversationInputKind::TaskGuidance,
                    ApplicationQueueItemKind::Unknown => ConversationInputKind::Unknown,
                },
            },
            ApplicationQueueAction::Edit {
                entry_id, prompt, ..
            } => Target::QueueEdit {
                queue_id: entry_id.as_str().to_owned(),
                prompt_hash: queue_prompt_hash(prompt.as_str()),
            },
            ApplicationQueueAction::Remove { entry_id, .. } => Target::QueueCancel {
                queue_id: entry_id.as_str().to_owned(),
            },
            ApplicationQueueAction::Move { entry_id, .. }
            | ApplicationQueueAction::Promote { entry_id, .. }
            | ApplicationQueueAction::Reorder { entry_id, .. } => Target::QueueReorder {
                queue_id: entry_id.as_str().to_owned(),
            },
            ApplicationQueueAction::Pause => Target::QueuePause { paused: true },
            ApplicationQueueAction::Resume => Target::QueuePause { paused: false },
            _ => return Ok(None),
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::SavePlan {
            plan_id,
            expected_plan_hash,
        }) => Target::PlanDecision {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
            decision: sigil_kernel::PlanDecision::SavedOnly,
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::RejectPlan {
            plan_id,
            expected_plan_hash,
        }) => Target::PlanDecision {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
            decision: sigil_kernel::PlanDecision::Rejected,
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::CreateTaskFromPlan {
            plan_id,
            expected_plan_hash,
            ..
        }) => Target::PlanAdoption {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::AdoptPlanCandidate {
            plan_id,
            expected_candidate_hash,
        }) => Target::PlanCandidateAdoption {
            plan_id: plan_id.as_str().to_owned(),
            candidate_hash: expected_candidate_hash.as_str().to_owned(),
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::RevisePlan {
            plan_id,
            expected_plan_hash,
        }) => Target::PlanRevisionGuidance {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
        },
        ApplicationCommand::UserInput(UserInputCommand::ResumeCommittedUserInput {
            binding,
            generation,
            expected_request_hash,
            ..
        }) => {
            let original =
                resumed_user_input_operation(request)?.ok_or(ApplicationError::ScopeMismatch)?;
            Target::UserInputContinuation {
                original_operation_id: original.operation_id,
                request_id: binding.clone(),
                generation: *generation,
                request_hash: expected_request_hash.as_str().to_owned(),
            }
        }
        ApplicationCommand::UserInput(UserInputCommand::Resolve {
            binding,
            generation,
            expected_request_hash,
            ..
        }) => Target::UserInputDecision {
            request_id: binding.clone(),
            generation: *generation,
            request_hash: expected_request_hash.as_str().to_owned(),
            command_id: request.envelope.command_id.as_str().to_owned(),
        },
        _ => return Ok(None),
    };
    let key = request
        .admission
        .reservation_key(&request.envelope.command_id);
    let key_digest = application_reservation_key_digest(&key)?;
    let scope = request
        .admission
        .scope
        .session
        .as_ref()
        .ok_or(ApplicationError::ScopeRequired)?
        .as_str()
        .to_owned();
    ApplicationOperationBindingV1::new(
        scope,
        key_digest,
        sigil_application::command_fingerprint(request)?,
        target,
    )
    .map(Some)
    .map_err(unavailable)
}

/// Canonical identity shared by original dispatch and typed continuation validation.
pub fn application_reservation_key_digest(
    key: &sigil_application::CommandReservationKey,
) -> Result<String, ApplicationError> {
    key.validate()?;
    let value = serde_json::to_value(key).map_err(unavailable)?;
    let canonical = sigil_kernel::canonicalize_cache_stable_json(&value).map_err(unavailable)?;
    Ok(sigil_kernel::sha256_hex(
        &serde_json::to_vec(&canonical).map_err(unavailable)?,
    ))
}

pub fn resumed_user_input_operation(
    request: &ApplicationCommandRequest,
) -> Result<Option<ApplicationOperationBindingV1>, ApplicationError> {
    let ApplicationCommand::UserInput(UserInputCommand::ResumeCommittedUserInput {
        original_key,
        original_fingerprint,
        original_operation_id,
        original_domain_session_scope_id,
        binding,
        generation,
        expected_request_hash,
    }) = &request.envelope.command
    else {
        return Ok(None);
    };
    if original_key.authority_scope != request.admission.scope
        || original_key.principal != request.admission.principal
        || original_key.command_id == request.envelope.command_id
    {
        return Err(ApplicationError::ScopeMismatch);
    }
    let mut operation = ApplicationOperationBindingV1::new(
        original_key
            .authority_scope
            .session
            .as_ref()
            .ok_or(ApplicationError::ScopeRequired)?
            .as_str()
            .to_owned(),
        application_reservation_key_digest(original_key)?,
        original_fingerprint.clone(),
        ApplicationOperationTargetV1::UserInputDecision {
            request_id: binding.clone(),
            generation: *generation,
            request_hash: expected_request_hash.as_str().to_owned(),
            command_id: original_key.command_id.as_str().to_owned(),
        },
    )
    .map_err(unavailable)?;
    operation.domain_session_scope_id = original_domain_session_scope_id.clone();
    operation.validate().map_err(unavailable)?;
    if &operation.operation_id != original_operation_id {
        return Err(ApplicationError::ScopeMismatch);
    }
    Ok(Some(operation))
}

fn unavailable(_: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Unavailable
}

fn queue_prompt_hash(raw: &str) -> String {
    sigil_kernel::project_conversation_prompt_for_persistence(raw).prompt_hash
}

fn queue_target(
    target: &ApplicationQueueTarget,
) -> Result<ConversationInputTarget, ApplicationError> {
    Ok(match target {
        ApplicationQueueTarget::MainThread => ConversationInputTarget::MainThread,
        ApplicationQueueTarget::AgentThread { thread_id } => ConversationInputTarget::AgentThread {
            thread_id: sigil_kernel::AgentThreadId::new(thread_id.as_str()).map_err(unavailable)?,
        },
        ApplicationQueueTarget::Task { task_id } => ConversationInputTarget::Task {
            task_id: sigil_kernel::TaskId::new(task_id.as_str()).map_err(unavailable)?,
        },
    })
}

/// Uses actual domain records, never a matching current value or transport acknowledgement.
pub fn reconcile_application_operation_receipt(
    request: &ApplicationCommandRequest,
    reader: &SessionRecordReadHandle,
    source_frontier: &ApplicationFrontier,
) -> Result<Option<crate::RuntimeApplicationDispatch>, ApplicationError> {
    let Some(binding) = application_operation_binding(request)? else {
        return Ok(None);
    };
    reconcile_resolved_application_operation_receipt(request, &binding, reader, source_frontier)
}

/// The resolved reader and binding are issued together by the original Session owner. The
/// parent frontier remains the application CAS; a child source has its own sequence space.
pub fn reconcile_resolved_application_operation_receipt(
    request: &ApplicationCommandRequest,
    binding: &ApplicationOperationBindingV1,
    reader: &SessionRecordReadHandle,
    source_frontier: &ApplicationFrontier,
) -> Result<Option<crate::RuntimeApplicationDispatch>, ApplicationError> {
    let Some(mut expected) = application_operation_binding(request)? else {
        return Ok(None);
    };
    expected.domain_session_scope_id = binding.domain_session_scope_id.clone();
    if &expected != binding {
        return Err(ApplicationError::ScopeMismatch);
    }
    let Some(proof) = sigil_kernel::session::reconcile_application_operation(reader, binding)
        .map_err(unavailable)?
    else {
        return Ok(None);
    };
    application_operation_receipt_from_proof(request, binding, &proof, source_frontier).map(Some)
}

pub fn application_operation_receipt_from_proof(
    request: &ApplicationCommandRequest,
    binding: &ApplicationOperationBindingV1,
    proof: &sigil_kernel::session::ApplicationOperationCommitProofV1,
    source_frontier: &ApplicationFrontier,
) -> Result<crate::RuntimeApplicationDispatch, ApplicationError> {
    let Some(mut expected) = application_operation_binding(request)? else {
        return Err(ApplicationError::ScopeMismatch);
    };
    expected.domain_session_scope_id = binding.domain_session_scope_id.clone();
    if &expected != binding {
        return Err(ApplicationError::ScopeMismatch);
    }
    proof.validate_binding(binding).map_err(unavailable)?;
    if source_frontier.scope != request.admission.scope
        || source_frontier.writer_generation != request.envelope.expected_frontier.writer_generation
        || proof.source_session_scope_id() != binding.domain_session_scope_id()
        || (binding.domain_session_scope_id() == binding.session_scope_id
            && source_frontier.through_sequence < proof.stream_sequence())
    {
        return Err(ApplicationError::ScopeMismatch);
    }
    let receipt = ApplicationDomainReceipt {
        command_id: request.envelope.command_id.clone(),
        command_kind: request.envelope.command.kind().to_owned(),
        frontier: source_frontier.clone(),
        settlement: request.envelope.command.policy().settlement,
        summary: "domain operation committed".to_owned(),
        domain_commit: ApplicationDomainCommitRef {
            source_session_scope_id: Some(proof.source_session_scope_id().to_owned()),
            source_event_id: proof.event_id().to_owned(),
            source_sequence: proof.stream_sequence(),
            source_digest: sigil_kernel::sha256_hex(proof.record_checksum().as_bytes()),
        },
        outcome: None,
    };
    receipt.validate_for(
        &request
            .admission
            .reservation_key(&request.envelope.command_id),
    )?;
    Ok(crate::RuntimeApplicationDispatch::Settled(receipt))
}
